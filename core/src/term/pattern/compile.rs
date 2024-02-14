//! Compilation of pattern matching down to pattern-less Nickel code.
//!
//! # Algorithm
//!
//! Compiling patterns amounts to generate a decision tree - concretely, a term composed mostly of
//! nested if-then-else - which either succeeds to match a value and returns the bindings of
//! pattern variables, or fails and returns `null`.
//!
//! Compilation of pattern matching is a well-studied problem in the literature, where efficient
//! algorithms try to avoid the duplication of checks by "grouping" them in a smart way. A standard
//! resource on this topic is the paper [_Compiling Pattern Matching to Good Decision
//! Trees_](https://dl.acm.org/doi/10.1145/1411304.1411311) by Luc Maranget.
//!
//! The current version of pattern compilation in Nickel is naive: it simply compiles each pattern
//! to a checking expression and tries them all until one works. We don't expect pattern matching
//! to be relevant for performance anytime soon (allegedly, there are much more impacting aspects
//! to handle before that). We might revisit this in the future if pattern matching turns out to be
//! a bottleneck.
//!
//! Most build blocks are generated programmatically rather than written out as e.g. members of the
//! [internals] stdlib module. While clunkier, this lets more easily change the compilation
//! strategy in the future and is already a more efficient in the current setting (combining
//! building blocks from the standard library would require much more function applications, while
//! we can generate inlined versions on-the-fly here).
use super::*;
use crate::{
    mk_app,
    term::{make, BinaryOp, MatchData, RecordExtKind, RecordOpKind, RichTerm, Term, UnaryOp},
};

/// Generate a standard `%record_insert` primop as generated by the parser.
fn record_insert() -> BinaryOp {
    BinaryOp::DynExtend {
        ext_kind: RecordExtKind::WithValue,
        metadata: Default::default(),
        pending_contracts: Default::default(),
        // We don't really care for optional fields here and we don't need to filter them out
        op_kind: RecordOpKind::ConsiderAllFields,
    }
}

/// Generate a Nickel expression which inserts a new binding in the working dictionary.
///
/// `%record_insert% "<id>" bindings_id value_id`
fn insert_binding(id: LocIdent, value_id: LocIdent, bindings_id: LocIdent) -> RichTerm {
    mk_app!(
        make::op2(
            record_insert(),
            Term::Str(id.label().into()),
            Term::Var(bindings_id)
        ),
        Term::Var(value_id)
    )
}

/// Generate a Nickel expression which update the `REST_FIELD` field of the working bindings by
/// remove the `field` from it.
///
/// ```nickel
/// %record_insert% "<REST_FIELD>"
///   (%record_remove% "<REST_FIELD>" bindings_id)
///   (%record_remove% "<field>"
///     (%static_access(REST_FIELD) bindings_id)
///   )
/// ```
fn remove_from_rest(rest_field: LocIdent, field: LocIdent, bindings_id: LocIdent) -> RichTerm {
    let rest = make::op1(UnaryOp::StaticAccess(rest_field), Term::Var(bindings_id));

    let rest_shrinked = make::op2(
        BinaryOp::DynRemove(RecordOpKind::ConsiderAllFields),
        Term::Str(field.label().into()),
        rest,
    );

    let bindings_shrinked = make::op2(
        BinaryOp::DynRemove(RecordOpKind::ConsiderAllFields),
        Term::Str(rest_field.into()),
        Term::Var(bindings_id),
    );

    mk_app!(
        make::op2(
            record_insert(),
            Term::Str(rest_field.into()),
            bindings_shrinked,
        ),
        rest_shrinked
    )
}

pub trait CompilePart {
    /// Compile part of a broader pattern to a Nickel expression with two free variables (which
    /// is equivalent to a function of two arguments):
    ///
    /// 1. The value being matched on (`value_id`)
    /// 2. A dictionary of the current assignment of pattern variables to sub-expressions of the
    ///    matched expression
    ///
    /// The compiled expression must return either `null` if the pattern doesn't match, or a
    /// dictionary mapping pattern variables to the corresponding sub-expressions of the
    /// matched value if the pattern matched with success.
    ///
    /// Although the `value` and `bindings` could be passed as [crate::term::RichTerm] in all
    /// generality, forcing them to be variable makes it less likely that the compilation
    /// duplicates sub-expressions: because the value and the bindings must always be passed in
    /// a variable, they are free to share without risk of duplicating work.
    fn compile_part(&self, value_id: LocIdent, bindings_id: LocIdent) -> RichTerm;
}

impl CompilePart for Pattern {
    // Compilation of the top-level pattern wrapper (code between < and > is Rust code, think
    // a template of some sort):
    //
    // < if let Some(alias) = alias { >
    //   let bindings = %record_insert% <alias> bindings arg in
    // < } >
    // <pattern_data.compile()> arg bindings
    fn compile_part(&self, value_id: LocIdent, bindings_id: LocIdent) -> RichTerm {
        // The last instruction
        // <pattern_data.compile()>
        let continuation = self.data.compile_part(value_id, bindings_id);

        // Either
        //
        // let bindings = %record_insert% <alias> bindings arg in
        // continuation
        //
        // if `alias` is set, or just `continuation` otherwise.
        if let Some(alias) = self.alias {
            make::let_in(
                bindings_id,
                insert_binding(alias, value_id, bindings_id),
                continuation,
            )
        } else {
            continuation
        }
    }
}

impl CompilePart for PatternData {
    fn compile_part(&self, value_id: LocIdent, bindings_id: LocIdent) -> RichTerm {
        match self {
            PatternData::Any(id) => {
                // %record_insert% "<id>" value_id bindings_id
                insert_binding(*id, value_id, bindings_id)
            }
            PatternData::Record(pat) => pat.compile_part(value_id, bindings_id),
            PatternData::Enum(pat) => pat.compile_part(value_id, bindings_id),
        }
    }
}

impl CompilePart for RecordPattern {
    // Compilation of the top-level record pattern wrapper.
    //
    // To check that the value doesn't contain extra fields, or to capture the rest of the
    // record when using the `..rest` construct, we need to remove matched fields from the
    // original value at each stage and thread this working value in addition to the bindings.
    //
    // We don't have tuples, and to avoid adding an indirection (by storing the current state
    // as `{rest, bindings}` where bindings itself is a record), we store this rest alongside
    // the bindings in a special field which is a freshly generated indentifier. This is an
    // implementation detail which isn't very hard to change, should we have to.
    //
    // if %typeof% value_id == 'Record
    //   let final_bindings_id =
    //
    // <fold (field, value) in fields
    //  - cont is the accumulator
    //  - initial accumulator is `%record_insert% "<REST_FIELD>" bindings_id value_id`
    //  >
    //    if %field_is_defined% field value_id then
    //      let local_bindings_id = cont in
    //
    //      if local_bindings_id == null then
    //        null
    //      else
    //        let local_value_id = %static_access(field)% (%static_access(REST_FIELD)% local_bindings_id)
    //        let local_bindings_id = <remove_from_rest(field, local_bindings_id)> in
    //        <field.compile_part(local_value_id, local_bindings_id)>
    //    else
    //      null
    //  <end fold>
    //
    //   in
    //
    //   <if self.tail is empty>
    //     # if tail is empty, check that the value doesn't contain extra fields
    //     if final_bindings_id == null ||
    //        (%static_access% <REST_FIELD> final_bindings_id) != {} then
    //       null
    //     else
    //       %record_remove% "<REST>" final_bindings_id
    //   <else if self.tail is capture(rest)>
    //   # move the rest from REST_FIELD to rest, and remove REST_FIELD
    //     if final_bindings_id == null then
    //       null
    //     else
    //       %record_remove% "<REST>"
    //         (%record_insert% <rest>
    //           final_bindings_id
    //           (%static_access% <REST_FIELD> final_bindings_id)
    //         )
    //   <else if self.tail is open>
    //     %record_remove% "<REST>" final_bindings_id
    //   <end if>
    // else
    //   null
    fn compile_part(&self, value_id: LocIdent, bindings_id: LocIdent) -> RichTerm {
        let rest_field = LocIdent::fresh();

        // `%record_insert% "<REST>" bindings_id value_id`
        let init_bindings = mk_app!(
            make::op2(
                record_insert(),
                Term::Str(rest_field.into()),
                Term::Var(bindings_id)
            ),
            Term::Var(value_id)
        );

        // The fold block:
        //
        // <fold (field, value) in fields
        //  - cont is the accumulator
        //  - initial accumulator is `%record_insert% "<REST>" bindings_id value_id`
        // >
        //
        //
        // if %field_is_defined% field value_id then
        //   let local_bindings_id = cont in
        //
        //   if local_bindings_id == null then
        //     null
        //   else
        //     let local_value_id = %static_access(field)% (%static_access(REST_FIELD)% local_bindings_id)
        //     let local_bindings_id = <remove_from_rest(field, local_bindings_id)> in
        //     <field.compile_part(local_value_id, local_bindings_id)>
        let fold_block: RichTerm = self.patterns.iter().fold(init_bindings, |cont, field_pat| {
            let field = field_pat.matched_id;
            let local_bindings_id = LocIdent::fresh();
            let local_value_id = LocIdent::fresh();

            // let bindings_id = <remove_from_rest(field, local_bindings_id)> in
            // <field.compile_part(local_value_id, local_bindings_id)>
            let updated_bindings_let = make::let_in(
                local_bindings_id,
                remove_from_rest(rest_field, field, local_bindings_id),
                field_pat
                    .pattern
                    .compile_part(local_value_id, local_bindings_id),
            );

            // let value_id = %static_access(field)% (%static_access(REST_FIELD)% local_bindings_id)
            // in <updated_bindings_let>
            let inner_else_block = make::let_in(
                local_value_id,
                make::op1(
                    UnaryOp::StaticAccess(field),
                    make::op1(
                        UnaryOp::StaticAccess(rest_field),
                        Term::Var(local_bindings_id),
                    ),
                ),
                updated_bindings_let,
            );

            // The innermost if:
            //
            // if local_bindings_id == null then
            //   null
            // else
            //  <inner_else_block>
            let inner_if = make::if_then_else(
                make::op2(BinaryOp::Eq(), Term::Var(local_bindings_id), Term::Null),
                Term::Null,
                inner_else_block,
            );

            // let local_bindings_id = cont in <value_let>
            let binding_cont_let = make::let_in(local_bindings_id, cont, inner_if);

            // %field_is_defined% field value_id
            let has_field = make::op2(
                BinaryOp::FieldIsDefined(RecordOpKind::ConsiderAllFields),
                Term::Str(field.label().into()),
                Term::Var(value_id),
            );

            make::if_then_else(has_field, binding_cont_let, Term::Null)
        });

        // %typeof% value_id == 'Record
        let is_record: RichTerm = make::op2(
            BinaryOp::Eq(),
            make::op1(UnaryOp::Typeof(), Term::Var(value_id)),
            Term::Enum("Record".into()),
        );

        let final_bindings_id = LocIdent::fresh();

        // %record_remove% "<REST>" final_bindings_id
        let bindings_without_rest = make::op2(
            BinaryOp::DynRemove(RecordOpKind::ConsiderAllFields),
            Term::Str(rest_field.into()),
            Term::Var(final_bindings_id),
        );

        // the last block of the outer if, which depends on the tail of the record pattern
        let tail_block = match self.tail {
            //   if final_bindings_id == null ||
            //      (%static_access% <REST_FIELD> final_bindings_id) != {} then
            //     null
            //   else
            //     %record_remove% "<REST>" final_bindings_id
            RecordPatternTail::Empty => make::if_then_else(
                mk_app!(
                    make::op1(
                        UnaryOp::BoolOr(),
                        make::op2(BinaryOp::Eq(), Term::Var(final_bindings_id), Term::Null)
                    ),
                    make::op1(
                        UnaryOp::BoolNot(),
                        make::op2(
                            BinaryOp::Eq(),
                            make::op1(
                                UnaryOp::StaticAccess(rest_field),
                                Term::Var(final_bindings_id)
                            ),
                            Term::Record(RecordData::empty())
                        )
                    )
                ),
                Term::Null,
                bindings_without_rest,
            ),
            // %record_remove% "<REST>" final_bindings_id
            RecordPatternTail::Open => bindings_without_rest,
            // if final_bindings_id == null then
            //   null
            // else
            //   %record_remove% "<REST>"
            //     (%record_insert% <rest>
            //       final_bindings_id
            //       (%static_access% <REST_FIELD> final_bindings_id)
            //     )
            RecordPatternTail::Capture(rest) => make::if_then_else(
                make::op2(BinaryOp::Eq(), Term::Var(final_bindings_id), Term::Null),
                Term::Null,
                make::op2(
                    BinaryOp::DynRemove(RecordOpKind::ConsiderAllFields),
                    Term::Str(rest_field.into()),
                    mk_app!(
                        make::op2(
                            record_insert(),
                            Term::Str(rest.label().into()),
                            Term::Var(final_bindings_id),
                        ),
                        make::op1(
                            UnaryOp::StaticAccess(rest_field),
                            Term::Var(final_bindings_id)
                        )
                    ),
                ),
            ),
        };

        // The let enclosing the fold block and the final block:
        // let final_bindings_id = <fold_block> in <tail_block>
        let outer_let = make::let_in(final_bindings_id, fold_block, tail_block);

        // if <is_record> then <outer_let> else null
        make::if_then_else(is_record, outer_let, Term::Null)
    }
}

impl CompilePart for EnumPattern {
    fn compile_part(&self, value_id: LocIdent, bindings_id: LocIdent) -> RichTerm {
        // %enum_get_tag% value_id == '<self.tag>
        let tag_matches = make::op2(
            BinaryOp::Eq(),
            make::op1(UnaryOp::EnumGetTag(), Term::Var(value_id)),
            Term::Enum(self.tag),
        );

        if let Some(pat) = &self.pattern {
            // if %enum_is_variant% value_id && %enum_get_tag% value_id == '<self.tag> then
            //   let value_id = %enum_unwrap_variant% value_id in
            //   <pattern.compile(value_id, bindings_id)>
            // else
            //   null

            // %enum_is_variant% value_id && <tag_matches>
            let if_condition = mk_app!(
                make::op1(
                    UnaryOp::BoolAnd(),
                    make::op1(UnaryOp::EnumIsVariant(), Term::Var(value_id)),
                ),
                tag_matches
            );

            make::if_then_else(
                if_condition,
                make::let_in(
                    value_id,
                    make::op1(UnaryOp::EnumUnwrapVariant(), Term::Var(value_id)),
                    pat.compile_part(value_id, bindings_id),
                ),
                Term::Null,
            )
        } else {
            // if %typeof% value_id == 'Enum && !(%enum_is_variant% value_id) && <tag_matches> then
            //   bindings_id
            // else
            //   null

            // %typeof% value_id == 'Enum
            let is_enum = make::op2(
                BinaryOp::Eq(),
                make::op1(UnaryOp::Typeof(), Term::Var(value_id)),
                Term::Enum("Enum".into()),
            );

            // !(%enum_is_variant% value_id)
            let is_enum_tag = make::op1(
                UnaryOp::BoolNot(),
                make::op1(UnaryOp::EnumIsVariant(), Term::Var(value_id)),
            );

            // <is_enum> && <is_enum_tag> && <tag_matches>
            let if_condition = mk_app!(
                make::op1(UnaryOp::BoolAnd(), is_enum,),
                mk_app!(make::op1(UnaryOp::BoolAnd(), is_enum_tag,), tag_matches)
            );

            make::if_then_else(if_condition, Term::Var(bindings_id), Term::Null)
        }
    }
}

pub trait Compile {
    /// Compile a match expression to a Nickel expression with the provided `value_id` as a
    /// free variable (representing a placeholder for the matched expression).
    fn compile(self, value: RichTerm, pos: TermPos) -> RichTerm;
}

impl Compile for MatchData {
    // Compilation of a full match expression (code between < and > is Rust code, think of it
    // as a kind of templating):
    //
    // let value_id = value in
    //
    // <for (pattern, body) in branches.rev()
    //  - cont is the accumulator
    //  - initial accumulator is the default branch (or error if not default branch)
    // >
    //    let init_bindings_id = {} in
    //    let bindings_id = <pattern.compile()> value_id init_bindings_id in
    //
    //    if bindings_id == null then
    //      cont
    //    else
    //      # this primop evaluates body with an environment extended with bindings_id
    //      %pattern_branch% body bindings_id
    fn compile(self, value: RichTerm, pos: TermPos) -> RichTerm {
        let default_branch = self.default.unwrap_or_else(|| {
            Term::RuntimeError(EvalError::NonExhaustiveMatch {
                value: value.clone(),
                pos,
            })
            .into()
        });
        let value_id = LocIdent::fresh();

        // The fold block:
        //
        // <for (pattern, body) in branches.rev()
        //  - cont is the accumulator
        //  - initial accumulator is the default branch (or error if not default branch)
        // >
        //    let init_bindings_id = {} in
        //    let bindings_id = <pattern.compile_part(value_id, init_bindings)> in
        //
        //    if bindings_id == null then
        //      cont
        //    else
        //      # this primop evaluates body with an environment extended with bindings_id
        //      %pattern_branch% body bindings_id
        let fold_block =
            self.branches
                .into_iter()
                .rev()
                .fold(default_branch, |cont, (pat, body)| {
                    let init_bindings_id = LocIdent::fresh();
                    let bindings_id = LocIdent::fresh();

                    // inner if block:
                    //
                    // if bindings_id == null then
                    //   cont
                    // else
                    //   # this primop evaluates body with an environment extended with bindings_id
                    //   %pattern_branch% bindings_id body
                    let inner = make::if_then_else(
                        make::op2(BinaryOp::Eq(), Term::Var(bindings_id), Term::Null),
                        cont,
                        mk_app!(
                            make::op1(UnaryOp::PatternBranch(), Term::Var(bindings_id),),
                            body
                        ),
                    );

                    // The two initial chained let-bindings:
                    //
                    // let init_bindings_id = {} in
                    // let bindings_id = <pattern.compile_part(value_id, init_bindings)> in
                    // <inner>
                    make::let_in(
                        init_bindings_id,
                        Term::Record(RecordData::empty()),
                        make::let_in(
                            bindings_id,
                            pat.compile_part(value_id, init_bindings_id),
                            inner,
                        ),
                    )
                });

        // let value_id = value in <fold_block>
        make::let_in(value_id, value, fold_block)
    }
}
