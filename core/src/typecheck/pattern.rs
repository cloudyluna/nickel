use crate::{
    error::TypecheckError,
    identifier::{Ident, LocIdent},
    mk_uty_record_row,
    term::pattern::*,
    typ::{EnumRowsF, RecordRowsF, TypeF},
};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypecheckMode {
    Walk,
    Enforce,
}

/// A list of pattern variables and their associated type.
pub type TypeBindings = Vec<(LocIdent, UnifType)>;

/// An element of a pattern path. A pattern path is a sequence of steps that can be used to
/// uniquely locate a sub-pattern within a pattern.
///
/// For example, in the pattern `{foo={bar='Baz arg}}`:
///
/// - The path of the full pattern within itself is the empty path.
/// - The path of the `arg` pattern is `[Field("foo"), Field("bar"), Variant]`.
#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash)]
pub enum PatternPathElem {
    Field(Ident),
    Variant,
}

pub type PatternPath = Vec<PatternPathElem>;

/// The working state of [PatternType::pattern_types_inj].
pub(super) struct PatTypeState<'a> {
    /// The list of pattern variables introduced so far and their inferred type.
    bindings: &'a mut TypeBindings,
    /// The list of enum row tail variables that are left open when typechecking a match expression.
    enum_open_tails: &'a mut Vec<(PatternPath, UnifEnumRows)>,
    /// Record, as a field path, the position of wildcard pattern encountered in a record. This
    /// impact the final type of the pattern, as a wildcard pattern makes the corresponding row
    /// open.
    wildcard_pat_paths: &'a mut HashSet<PatternPath>,
}

/// Return value of [PatternTypes::pattern_types], which stores the overall type of a pattern,
/// together with the type of its bindings and additional information for the typechecking of match
/// expressions.
#[derive(Debug, Clone)]
pub struct PatternTypeData<T> {
    /// The type of the pattern.
    pub typ: T,
    /// A list of pattern variables and their associated type.
    pub bindings: Vec<(LocIdent, UnifType)>,
    /// A list of enum row tail variables that are left open when typechecking a match expression.
    ///
    /// Those variables (or their descendent in a row type) might need to be closed after the type
    /// of all the patterns of a match expression have been unified, depending on the presence of a
    /// wildcard pattern. The path of the corresponding sub-pattern is stored as well, since enum
    /// patterns in different positions might need different treatment. For example:
    ///
    /// ```nickel
    /// match {
    ///   'Foo ('Bar x) => <exp>,
    ///   'Foo ('Qux x) => <exp>,
    ///   _ => <exp>
    /// }
    /// ```
    ///
    /// The presence of a default case means that the row variables of top-level enum patterns
    /// might stay open. However, the type corresponding to the sub-patterns `'Bar x` and `'Qux x`
    /// must be closed, because this match expression can't handle `'Foo ('Other 0)`. The type of
    /// the match expression is thus `[| 'Foo [| 'Bar: a, 'Qux: b |]; c|] -> d`.
    ///
    /// Wildcard can occur anywhere, so the previous case can also happen within a record pattern:
    ///
    /// ```nickel
    /// match {
    ///   {foo = 'Bar x} => <exp>,
    ///   {foo = 'Qux x} => <exp>,
    ///   {foo = _} => <exp>,
    /// }
    /// ```
    ///
    /// Similarly, the type of the match expression is `{ foo: [| 'Bar: a, 'Qux: b; c |] } -> e`.
    ///
    /// See [^typechecking-match-expression] in [typecheck] for more details.
    pub enum_open_tails: Vec<(PatternPath, UnifEnumRows)>,
    /// Paths of the occurrence of wildcard patterns encountered. This is used to determine which
    /// tails in [Self::enum_open_tails] should be left open.
    pub wildcard_occurrences: HashSet<PatternPath>,
}

/// Close all the enum row types left open when typechecking a match expression. Special case of
/// `close_enums` for a single destructuring pattern (thus, where wildcard occurrences are not
/// relevant).
pub fn close_all_enums(enum_open_tails: Vec<(PatternPath, UnifEnumRows)>, state: &mut State) {
    close_enums(enum_open_tails, &HashSet::new(), state);
}

/// Close all the enum row types left open when typechecking a match expression, unless we recorded
/// a wildcard pattern somewhere in the same position.
pub fn close_enums(
    enum_open_tails: Vec<(PatternPath, UnifEnumRows)>,
    wildcard_occurrences: &HashSet<PatternPath>,
    state: &mut State,
) {
    // Note: both for this function and for `close_enums`, for a given pattern path, all the tail
    // variables should ultimately be part of the same enum type, and we just need to close it
    // once. We might thus save a bit of work if we kept equivalence classes of tuples (path, tail)
    // (equality being given by the equality of paths). Closing one arbitrary member per class
    // should then be enough. It's not obvious that this would make any difference in practice,
    // though.
    for tail in enum_open_tails
        .into_iter()
        .filter_map(|(path, tail)| (!wildcard_occurrences.contains(&path)).then_some(tail))
    {
        close_enum(tail, state);
    }
}

/// Take an enum row, find its final tail (in case of multiple indirection through unification
/// variables) and close it if it's a free unification variable.
fn close_enum(tail: UnifEnumRows, state: &mut State) {
    let root = tail.into_root(state.table);

    if let UnifEnumRows::UnifVar { id, .. } = root {
        // We don't need to perform any variable level checks when unifying a free
        // unification variable with a ground type
        state
            .table
            .assign_erows(id, UnifEnumRows::concrete(EnumRowsF::Empty));
    } else {
        let tail = root.iter().find_map(|row_item| {
            match row_item {
                GenericUnifEnumRowsIteratorItem::TailUnifVar { id, init_level } => {
                    Some(UnifEnumRows::UnifVar { id, init_level })
                }
                GenericUnifEnumRowsIteratorItem::TailVar(_)
                | GenericUnifEnumRowsIteratorItem::TailConstant(_) => {
                    // While unifying open enum rows coming from a pattern, we expect to always
                    // extend the enum row with other open rows such that the result should always
                    // stay open. So we expect to find a unification variable at the end of the
                    // enum row.
                    //
                    // But in fact, all the tails for a given pattern path will point to the same
                    // enum row, so it might have been closed already by a previous call to
                    // `close_enum`, and that's fine. On the other hand, we should never encounter
                    // a rigid type variable here (or a non-substituted type variable, although it
                    // has nothing to do with patterns), so if we reach this point, something is
                    // wrong with the typechecking of match expression.
                    debug_assert!(false);

                    None
                }
                _ => None,
            }
        });

        if let Some(tail) = tail {
            close_enum(tail, state)
        }
    }
}

pub(super) trait PatternTypes {
    /// The type produced by the pattern. Depending on the nature of the pattern, this type may
    /// vary: for example, a record pattern will produce record rows, while a general pattern will
    /// produce a general [super::UnifType]
    type PatType;

    /// Builds the type associated to the whole pattern, as well as the types associated to each
    /// binding introduced by this pattern. When matching a value against a pattern in a statically
    /// typed code, either by destructuring or by applying a match expression, the type of the
    /// value will be checked against the type generated by `pattern_type` and the bindings will be
    /// added to the type environment.
    ///
    /// The type of each "leaf" identifier will be assigned based on the `mode` argument. The
    /// current possibilities are for each leaf to have type `Dyn`, to use an explicit type
    /// annotation, or to be assigned a fresh unification variable.
    fn pattern_types(
        &self,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<PatternTypeData<Self::PatType>, TypecheckError> {
        let mut bindings = Vec::new();
        let mut enum_open_tails = Vec::new();
        let mut wildcard_pat_paths = HashSet::new();

        let typ = self.pattern_types_inj(
            &mut PatTypeState {
                bindings: &mut bindings,
                enum_open_tails: &mut enum_open_tails,
                wildcard_pat_paths: &mut wildcard_pat_paths,
            },
            Vec::new(),
            state,
            ctxt,
            mode,
        )?;

        Ok(PatternTypeData {
            typ,
            bindings,
            enum_open_tails,
            wildcard_occurrences: wildcard_pat_paths,
        })
    }

    /// Same as `pattern_types`, but inject the bindings in a working vector instead of returning
    /// them. Implementors should implement this method whose signature avoids creating and
    /// combining many short-lived vectors when walking recursively through a pattern.
    fn pattern_types_inj(
        &self,
        pt_state: &mut PatTypeState,
        path: PatternPath,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError>;
}

/// Builds the type associated to a record pattern. When matching a value against a pattern in a
/// statically typed code, for example in a let destructuring or via a match expression, the type
/// of the value will be checked against the type generated by `build_pattern_type`.
///
/// The type of each "leaf" identifier will be assigned based on the `mode` argument. The current
/// possibilities are for each leaf to have type `Dyn`, to use an explicit type annotation, or to
/// be assigned a fresh unification variable.
impl PatternTypes for RecordPattern {
    type PatType = UnifRecordRows;

    fn pattern_types_inj(
        &self,
        pt_state: &mut PatTypeState,
        path: PatternPath,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError> {
        let tail = if self.is_open() {
            match mode {
                // We use a dynamic tail here since we're in walk mode,
                // but if/when we remove dynamic record tails this could
                // likely be made an empty tail with no impact.
                TypecheckMode::Walk => mk_uty_record_row!(; RecordRowsF::TailDyn),
                TypecheckMode::Enforce => state.table.fresh_rrows_uvar(ctxt.var_level),
            }
        } else {
            UnifRecordRows::Concrete {
                rrows: RecordRowsF::Empty,
                var_levels_data: VarLevelsData::new_no_uvars(),
            }
        };

        if let RecordPatternTail::Capture(rest) = self.tail {
            pt_state
                .bindings
                .push((rest, UnifType::concrete(TypeF::Record(tail.clone()))));
        }

        self.patterns
            .iter()
            .map(|field_pat| field_pat.pattern_types_inj(pt_state, path.clone(), state, ctxt, mode))
            .try_fold(tail, |tail, row: Result<UnifRecordRow, TypecheckError>| {
                Ok(UnifRecordRows::concrete(RecordRowsF::Extend {
                    row: row?,
                    tail: Box::new(tail),
                }))
            })
    }
}

impl PatternTypes for Pattern {
    type PatType = UnifType;

    fn pattern_types_inj(
        &self,
        pt_state: &mut PatTypeState,
        path: PatternPath,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError> {
        let typ = self
            .data
            .pattern_types_inj(pt_state, path, state, ctxt, mode)?;

        if let Some(alias) = self.alias {
            pt_state.bindings.push((alias, typ.clone()));
        }

        Ok(typ)
    }
}

// Depending on the mode, returns the type affected to patterns that match any value (`Any` and
// `Wildcard`): `Dyn` in walk mode, a fresh unification variable in enforce mode.
fn any_type(mode: TypecheckMode, state: &mut State, ctxt: &Context) -> UnifType {
    match mode {
        TypecheckMode::Walk => mk_uniftype::dynamic(),
        TypecheckMode::Enforce => state.table.fresh_type_uvar(ctxt.var_level),
    }
}

impl PatternTypes for PatternData {
    type PatType = UnifType;

    fn pattern_types_inj(
        &self,
        pt_state: &mut PatTypeState,
        path: PatternPath,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError> {
        match self {
            PatternData::Wildcard => {
                pt_state.wildcard_pat_paths.insert(path);
                Ok(any_type(mode, state, ctxt))
            }
            PatternData::Any(id) => {
                let typ = any_type(mode, state, ctxt);
                pt_state.bindings.push((*id, typ.clone()));

                Ok(typ)
            }
            PatternData::Record(record_pat) => Ok(UnifType::concrete(TypeF::Record(
                record_pat.pattern_types_inj(pt_state, path, state, ctxt, mode)?,
            ))),
            PatternData::Enum(enum_pat) => {
                let row = enum_pat.pattern_types_inj(pt_state, path.clone(), state, ctxt, mode)?;
                // We elaborate the type `[| row; a |]` where `a` is a fresh enum rows unification
                // variable registered in `enum_open_tails`.
                let tail = state.table.fresh_erows_uvar(ctxt.var_level);
                pt_state.enum_open_tails.push((path, tail.clone()));

                Ok(UnifType::concrete(TypeF::Enum(UnifEnumRows::concrete(
                    EnumRowsF::Extend {
                        row,
                        tail: Box::new(tail),
                    },
                ))))
            }
            PatternData::Constant(constant_pat) => {
                constant_pat.pattern_types_inj(pt_state, path, state, ctxt, mode)
            }
        }
    }
}

impl PatternTypes for ConstantPattern {
    type PatType = UnifType;

    fn pattern_types_inj(
        &self,
        pt_state: &mut PatTypeState,
        path: PatternPath,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError> {
        self.data
            .pattern_types_inj(pt_state, path, state, ctxt, mode)
    }
}

impl PatternTypes for ConstantPatternData {
    type PatType = UnifType;

    fn pattern_types_inj(
        &self,
        _pt_state: &mut PatTypeState,
        _path: PatternPath,
        _state: &mut State,
        _ctxt: &Context,
        _mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError> {
        Ok(match self {
            ConstantPatternData::Bool(_) => UnifType::concrete(TypeF::Bool),
            ConstantPatternData::Number(_) => UnifType::concrete(TypeF::Number),
            ConstantPatternData::String(_) => UnifType::concrete(TypeF::String),
            ConstantPatternData::Null => UnifType::concrete(TypeF::Dyn),
        })
    }
}

impl PatternTypes for FieldPattern {
    type PatType = UnifRecordRow;

    fn pattern_types_inj(
        &self,
        pt_state: &mut PatTypeState,
        mut path: PatternPath,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError> {
        path.push(PatternPathElem::Field(self.matched_id.ident()));

        // If there is a static type annotations in a nested record patterns then we need to unify
        // them with the pattern type we've built to ensure (1) that they're mutually compatible
        // and (2) that we assign the annotated types to the right unification variables.
        let ty_row = match (&self.annotation.typ, &self.pattern.data, mode) {
            // However, in walk mode, we only do that when the nested pattern isn't a leaf (i.e.
            // `Any` or `Wildcard`) for backward-compatibility reasons.
            //
            // Before this function was refactored, Nickel has been allowing things like `let {foo
            // : Number} = {foo = 1} in foo` in walk mode, which would fail to typecheck with the
            // generic approach: the pattern is parsed as `{foo : Number = foo}`, the second
            // occurrence of `foo` gets type `Dyn` in walk mode, but `Dyn` fails to unify with
            // `Number`. In this case, we don't recursively call `pattern_types_inj` in the first
            // place and just declare that the type of `foo` is `Number`.
            //
            // This special case should probably be ruled out, requiring the users to use `let {foo
            // | Number}` instead, at least outside of a statically typed code block. But before
            // this happens, we special case the old behavior and eschew unification.
            (Some(annot_ty), PatternData::Any(id), TypecheckMode::Walk) => {
                let ty_row = UnifType::from_type(annot_ty.typ.clone(), &ctxt.term_env);
                pt_state.bindings.push((*id, ty_row.clone()));
                ty_row
            }
            (Some(annot_ty), PatternData::Wildcard, TypecheckMode::Walk) => {
                UnifType::from_type(annot_ty.typ.clone(), &ctxt.term_env)
            }
            (Some(annot_ty), _, _) => {
                let pos = annot_ty.typ.pos;
                let annot_uty = UnifType::from_type(annot_ty.typ.clone(), &ctxt.term_env);

                let ty_row = self
                    .pattern
                    .pattern_types_inj(pt_state, path, state, ctxt, mode)?;

                ty_row
                    .clone()
                    .unify(annot_uty, state, ctxt)
                    .map_err(|e| e.into_typecheck_err(state, pos))?;

                ty_row
            }
            _ => self
                .pattern
                .pattern_types_inj(pt_state, path, state, ctxt, mode)?,
        };

        Ok(UnifRecordRow {
            id: self.matched_id,
            typ: Box::new(ty_row),
        })
    }
}

impl PatternTypes for EnumPattern {
    type PatType = UnifEnumRow;

    fn pattern_types_inj(
        &self,
        pt_state: &mut PatTypeState,
        mut path: PatternPath,
        state: &mut State,
        ctxt: &Context,
        mode: TypecheckMode,
    ) -> Result<Self::PatType, TypecheckError> {
        let typ_arg = self
            .pattern
            .as_ref()
            .map(|pat| {
                path.push(PatternPathElem::Variant);
                pat.pattern_types_inj(pt_state, path, state, ctxt, mode)
            })
            .transpose()?
            .map(Box::new);

        Ok(UnifEnumRow {
            id: self.tag,
            typ: typ_arg,
        })
    }
}
