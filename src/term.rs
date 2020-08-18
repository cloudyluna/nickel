//! AST of a Nickel expression.
//!
//! # Core language
//!
//! At its core, Nickel is a lazy JSON with higher-order functions. It includes:
//! - Basic values: booleans, numerals, string
//! - Data structures: lists and records
//! - Binders: functions and let bindings
//!
//! It also features type annotations (promise and assume), and other typechecking related
//! constructs (label, symbols, etc.).
//!
//! # Enriched values
//!
//! Enriched values are special terms used to represent metadata about record fields: types or
//! contracts, default values, documentation, etc. They bring such usually external object down to
//! the term level, and together with [merge](../merge/index.html), they allow for flexible and
//! modular definitions of contracts, record and metadata all together.
use crate::identifier::Ident;
use crate::label::Label;
use crate::position::RawSpan;
use crate::types::Types;
use std::collections::HashMap;

/// The AST of a Nickel expression.
///
/// Parsed terms also need to store their position in the source for error reporting.  This is why
/// this type is nested with [`RichTerm`](type.RichTerm.html).
///
#[derive(Debug, PartialEq, Clone)]
pub enum Term {
    /// A boolean value.
    Bool(bool),
    /// A floating-point value.
    Num(f64),
    /// A literal string.
    Str(String),
    /// A function.
    Fun(Ident, RichTerm),
    /// A blame label.
    Lbl(Label),

    /// A let binding.
    Let(Ident, RichTerm, RichTerm),
    /// An application.
    App(RichTerm, RichTerm),
    /// A variable.
    Var(Ident),

    /// An enum variant.
    Enum(Ident),

    /// A record, mapping identifiers to terms.
    Record(HashMap<Ident, RichTerm>),

    /// A list.
    List(Vec<RichTerm>),

    /// A primitive unary operator.
    Op1(UnaryOp<RichTerm>, RichTerm),
    /// A primitive binary operator.
    Op2(BinaryOp<RichTerm>, RichTerm, RichTerm),

    /// A promise.
    ///
    /// Represent a subterm which is to be statically typechecked.
    Promise(Types, Label, RichTerm),

    /// An assume.
    ///
    /// Represent a subterm which is to be dynamically typechecked (dynamic types are also called.
    /// It ensures at runtime that the term satisfies the contract corresponding to the type, or it
    /// will blame the label instead.
    Assume(Types, Label, RichTerm),

    /// A symbol.
    ///
    /// A unique tag corresponding to a type variable. See `Wrapped` below.
    Sym(i32),

    /// A wrapped term.
    ///
    /// Wrapped terms are introduced by contracts on polymorphic types. Take the following example:
    ///
    /// ```
    /// let f = Assume(forall a. forall b. a -> b -> a, fun x y => y) in
    /// f true "a"
    /// ```
    ///
    /// This function is ill-typed. To check that, a polymorphic contract will:
    /// - Assign a unique identifier to each type variable: say `a => 1`, `b => 2`
    /// - For each cast on a negative occurrence of a type variable `a` or `b` (corresponding to an
    /// argument position), tag the argument with the associated identifier. In our example, `f
    /// true "a"` will push `Wrapped(1, true)` then `Wrapped(2, "a")` on the stack.
    /// - For each cast on a positive occurrence of a type variable, this contract check that the
    /// term is of the form `Wrapped(id, term)` where `id` corresponds to the identifier of the
    /// type variable. In our example, the last cast to `a` finds `Wrapped(2, "a")`, while it
    /// expected `Wrapped(1, _)`, hence it raises a positive blame.
    Wrapped(i32, RichTerm),

    /// A contract. Enriched value.
    ///
    /// A contract at the term level. This contract is enforced when merged with a value.
    Contract(Types, Label),

    /// A default value. Enriched value.
    ///
    /// An enriched term representing a default value. It is dropped as soon as it is merged with a
    /// concrete value. Otherwise, if it lives long enough to be accessed, it evaluates to the
    /// underlying term.
    DefaultValue(RichTerm),

    /// A contract with combined with default value. Enriched value.
    ///
    /// This is a combination generated during evaluation, when merging a contract and a default
    /// value, as both need to be remembered.
    ContractWithDefault(Types, Label, RichTerm),

    /// A term together with its documentation string. Enriched value.
    Docstring(String, RichTerm),
}

impl Term {
    /// Recursively apply a function to all `Term`s contained in a `RichTerm`.
    pub fn apply_to_rich_terms<F>(&mut self, func: F)
    where
        F: Fn(&mut RichTerm),
    {
        use self::Term::*;
        match self {
            Op1(UnaryOp::Switch(ref mut map, ref mut def), ref mut t) => {
                map.iter_mut().for_each(|e| {
                    let (_, t) = e;
                    func(t);
                });
                func(t);
                if let Some(def) = def {
                    func(def)
                }
            }
            Record(ref mut static_map) => {
                static_map.iter_mut().for_each(|e| {
                    let (_, t) = e;
                    func(t);
                });
            }
            Op2(BinaryOp::DynExtend(ref mut t), ref mut t1, ref mut t2) => {
                func(t);
                func(t1);
                func(t2)
            }

            Bool(_) | Num(_) | Str(_) | Lbl(_) | Var(_) | Sym(_) | Enum(_) | Contract(_, _) => {}
            Fun(_, ref mut t)
            | Op1(_, ref mut t)
            | Promise(_, _, ref mut t)
            | Assume(_, _, ref mut t)
            | Wrapped(_, ref mut t)
            | DefaultValue(ref mut t)
            | Docstring(_, ref mut t)
            | ContractWithDefault(_, _, ref mut t) => {
                func(t);
            }
            Let(_, ref mut t1, ref mut t2)
            | App(ref mut t1, ref mut t2)
            | Op2(_, ref mut t1, ref mut t2) => {
                func(t1);
                func(t2);
            }
            List(ref mut terms) => terms.iter_mut().for_each(|t| {
                func(t);
            }),
        }
    }

    /// Return the class of an expression in WHNF.
    ///
    /// The class of an expression is an approximation of its type used in error reporting. Class
    /// and type coincide for constants (numbers, strings and booleans) and lists. Otherwise the
    /// class is less precise than the type and indicates the general shape of the term: `"Record"`
    /// for records, `"Fun`" for functions, etc. If the term is not a WHNF, `None` is returned.
    pub fn type_of(&self) -> Option<String> {
        match self {
            Term::Bool(_) => Some("Bool"),
            Term::Num(_) => Some("Num"),
            Term::Str(_) => Some("Str"),
            Term::Fun(_, _) => Some("Fun"),
            Term::Lbl(_) => Some("Label"),
            Term::Enum(_) => Some("Enum"),
            Term::Record(_) => Some("Record"),
            Term::List(_) => Some("List"),
            Term::Sym(_) => Some("Sym"),
            Term::Wrapped(_, _) => Some("Wrapped"),
            Term::Contract(_, _)
            | Term::ContractWithDefault(_, _, _)
            | Term::Docstring(_, _)
            | Term::DefaultValue(_) => Some("EnrichedValue"),
            Term::Let(_, _, _)
            | Term::App(_, _)
            | Term::Var(_)
            | Term::Op1(_, _)
            | Term::Op2(_, _, _)
            | Term::Promise(_, _, _)
            | Term::Assume(_, _, _) => None,
        }
        .map(|s| String::from(s))
    }

    /// Return a shallow string representation of a term, used for error reporting.
    pub fn shallow_repr(&self) -> String {
        match self {
            Term::Bool(true) => String::from("true"),
            Term::Bool(false) => String::from("false"),
            Term::Num(n) => format!("{}", n),
            Term::Str(s) => format!("\"{}\"", s),
            Term::Fun(_, _) => String::from("<func>"),
            Term::Lbl(_) => String::from("<label>"),
            Term::Enum(Ident(s)) => format!("`{}", s),
            Term::Record(_) => String::from("{ ... }"),
            Term::List(_) => String::from("[ ... ]"),
            Term::Sym(_) => String::from("<sym>"),
            Term::Wrapped(_, _) => String::from("<wrapped>"),
            Term::Contract(_, _) => String::from("<enriched:contract>"),
            Term::ContractWithDefault(_, _, ref t) => {
                format!("<enriched:contract,default={}>", (*t.term).shallow_repr())
            }
            Term::Docstring(_, ref t) => {
                format!("<enriched:doc,term={}>", (*t.term).shallow_repr())
            }
            Term::DefaultValue(ref t) => format!("<enriched:default={}", (*t.term).shallow_repr()),
            Term::Let(_, _, _)
            | Term::App(_, _)
            | Term::Var(_)
            | Term::Op1(_, _)
            | Term::Op2(_, _, _)
            | Term::Promise(_, _, _)
            | Term::Assume(_, _, _) => String::from("<unevaluated>"),
        }
    }

    /// Determine if a term is in evaluated from, called weak head normal form (WHNF).
    pub fn is_whnf(&self) -> bool {
        match self {
            Term::Bool(_)
            | Term::Num(_)
            | Term::Str(_)
            | Term::Fun(_, _)
            | Term::Lbl(_)
            | Term::Enum(_)
            | Term::Record(_)
            | Term::List(_)
            | Term::Sym(_) => true,
            Term::Let(_, _, _)
            | Term::App(_, _)
            | Term::Var(_)
            | Term::Op1(_, _)
            | Term::Op2(_, _, _)
            | Term::Promise(_, _, _)
            | Term::Assume(_, _, _)
            | Term::Wrapped(_, _)
            | Term::Contract(_, _)
            | Term::DefaultValue(_)
            | Term::ContractWithDefault(_, _, _)
            | Term::Docstring(_, _) => false,
        }
    }

    /// Determine if a term is an enriched value.
    pub fn is_enriched(&self) -> bool {
        match self {
            Term::Contract(_, _)
            | Term::DefaultValue(_)
            | Term::ContractWithDefault(_, _, _)
            | Term::Docstring(_, _) => true,
            Term::Bool(_)
            | Term::Num(_)
            | Term::Str(_)
            | Term::Fun(_, _)
            | Term::Lbl(_)
            | Term::Enum(_)
            | Term::Record(_)
            | Term::List(_)
            | Term::Sym(_)
            | Term::Wrapped(_, _)
            | Term::Let(_, _, _)
            | Term::App(_, _)
            | Term::Var(_)
            | Term::Op1(_, _)
            | Term::Op2(_, _, _)
            | Term::Promise(_, _, _)
            | Term::Assume(_, _, _) => false,
        }
    }
}

/// Primitive unary operators.
///
/// Some operators, such as if-then-else or `seq`, actually take several arguments but are only
/// strict in one (the tested boolean for example, in the case of if-then-else). They are encoded
/// as unary operators of this argument: indeed, in an expression `if-then-else boolean thenBlock
/// elseBlock`, `if-then-else` can be seen as a unary operator taking a `Bool` argument and
/// evaluating to either the first projection `fun x y => x` or the second projection `fun x y =>
/// y`.
#[derive(Clone, Debug, PartialEq)]
pub enum UnaryOp<CapturedTerm> {
    /// If-then-else.
    Ite(),

    /// Test if a number is zero.
    ///
    /// Will be removed once there is a reasonable equality.
    IsZero(),

    /// Test if a term is a numeral.
    IsNum(),
    /// Test if a term is a boolean.
    IsBool(),
    /// Test if a term is string literal.
    IsStr(),
    /// Test if a term is a function.
    IsFun(),
    /// Test if a term is a list.
    IsList(),

    /// Raise a blame, which stops the execution and prints an error according to the label argument.
    Blame(),

    /// Typecast an enum to a larger enum type.
    ///
    /// `Embed` is used to upcast enums. For example, if a value `x` has enum type `a | b`, then
    /// `embed c x` will have enum type `a | b | c`. It only affects typechecking as at runtime
    /// `embed someId` act like the identity.
    Embed(Ident),
    // This is a hacky way to deal with this for now.
    //
    // Ideally it should change to eliminate the dependency with RichTerm
    // in the future.
    /// A switch block. Used to match on a enumeration.
    Switch(HashMap<Ident, CapturedTerm>, Option<CapturedTerm>),

    /// Static access to a record field.
    ///
    /// Static means that the field identifier is a statically known string inside the source.
    StaticAccess(Ident),

    /// Map a function on a record.
    ///
    /// The mapped function must take two arguments, the name of the field as a string, and the
    /// content of the field. `MapRec` then replaces the content of each field by the result of the
    /// function: i.e., `mapRec f {a=2;}` evaluates to `{a=(f "a" 2);}`.
    MapRec(CapturedTerm),

    /// Inverse the polarity of a label.
    ChangePolarity(),

    /// Get the polarity of a label.
    Pol(),
    /// Go to the domain in the type path of a label.
    ///
    /// If the argument is a label with a [type path](../label/enum.TyPath.html) representing some
    /// subtype of the type of the original contract, as in:
    ///
    /// ```
    /// (Num -> Num) -> Num
    ///  ^^^^^^^^^^ type path
    /// ------------------- original type
    /// ```
    ///
    /// Then `GoDom` evaluates to a copy of this label, where the path has gone forward into the domain:
    ///
    /// ```
    /// (Num -> Num) -> Num
    ///  ^^^ new type path
    /// ------------------- original type
    /// ```
    GoDom(),
    /// Go to the codomain in the type path of a label.
    ///
    /// See `GoDom`.
    GoCodom(),
    /// Append text to the tag of a label.
    Tag(String),

    /// Wrap a term with a type tag (see `Wrapped` in [`Term`](enum.Term.html)).
    Wrap(),

    /// Force the evaluation of its argument and proceed with the second.
    Seq(),
    /// Recursively force the evaluation of its first argument then returns the second.
    ///
    /// Recursive here means that the evaluation does not stop at a WHNF, but the content of lists
    /// and records is also recursively forced.
    DeepSeq(),

    /// Return the head of a list.
    ListHead(),
    /// Return the tail of a list.
    ListTail(),
    /// Return the length of a list.
    ListLength(),
}

impl<Ty> UnaryOp<Ty> {
    pub fn map<To, F: Fn(Ty) -> To>(self, f: F) -> UnaryOp<To> {
        use UnaryOp::*;

        match self {
            Switch(m, op) => Switch(
                m.into_iter()
                    .map(|e| {
                        let (id, t) = e;
                        (id, f(t))
                    })
                    .collect(),
                op.map(f),
            ),
            MapRec(t) => MapRec(f(t)),

            Ite() => Ite(),

            IsZero() => IsZero(),

            IsNum() => IsNum(),
            IsBool() => IsBool(),
            IsStr() => IsStr(),
            IsFun() => IsFun(),
            IsList() => IsList(),

            Blame() => Blame(),

            Embed(id) => Embed(id),

            StaticAccess(id) => StaticAccess(id),

            ChangePolarity() => ChangePolarity(),
            Pol() => Pol(),
            GoDom() => GoDom(),
            GoCodom() => GoCodom(),
            Tag(s) => Tag(s),

            Wrap() => Wrap(),

            Seq() => Seq(),
            DeepSeq() => DeepSeq(),

            ListHead() => ListHead(),
            ListTail() => ListTail(),
            ListLength() => ListLength(),
        }
    }
}

/// Primitive binary operators
#[derive(Clone, Debug, PartialEq)]
pub enum BinaryOp<CapturedTerm> {
    /// Addition of numerals.
    Plus(),
    /// Concatenation of strings.
    PlusStr(),
    /// Unwrap a tagged term.
    ///
    /// See `Wrap` in [`UnaryOp`](enum.UnaryOp.html).
    Unwrap(),
    /// Equality on booleans.
    EqBool(),
    /// Extend a record with a dynamic field.
    ///
    /// Dynamic means that the field name may be an expression and not a statically known string.
    /// `DynExtend` tries to evaluate this name to a string, and in case of success, add a field
    /// with this name to the given record with the `CapturedTerm` as content.
    DynExtend(CapturedTerm),
    /// Remove a field from a record. The field name is given as an arbitrary Nickel expression.
    DynRemove(),
    /// Access the field of record. The field name is given as an arbitrary Nickel expression.
    DynAccess(),
    /// Test if a record has a specific field.
    HasField(),
    /// Concatenate two lists.
    ListConcat(),
    /// Map a function on each element of a list.
    ListMap(),
    /// Access the n-th element of a list.
    ListElemAt(),
    /// The merge operator (see the [merge module](../merge/index.html)).
    Merge(),
}

impl<Ty> BinaryOp<Ty> {
    pub fn map<To, F: Fn(Ty) -> To>(self, f: F) -> BinaryOp<To> {
        use BinaryOp::*;

        match self {
            DynExtend(t) => DynExtend(f(t)),
            Plus() => Plus(),
            PlusStr() => PlusStr(),
            Unwrap() => Unwrap(),
            EqBool() => EqBool(),
            DynRemove() => DynRemove(),
            DynAccess() => DynAccess(),
            HasField() => HasField(),
            ListConcat() => ListConcat(),
            ListMap() => ListMap(),
            ListElemAt() => ListElemAt(),
            Merge() => Merge(),
        }
    }

    pub fn is_strict(&self) -> bool {
        match self {
            BinaryOp::Merge() => false,
            _ => true,
        }
    }
}

/// Wrap [terms](type.Term.html) with positional information.
#[derive(Debug, PartialEq, Clone)]
pub struct RichTerm {
    pub term: Box<Term>,
    pub pos: Option<RawSpan>,
}

impl RichTerm {
    pub fn new(t: Term) -> RichTerm {
        RichTerm {
            term: Box::new(t),
            pos: None,
        }
    }

    /// Erase recursively the positional information.
    ///
    /// It allows to use rust `Eq` trait to compare the values of the underlying terms.
    pub fn clean_pos(&mut self) {
        self.pos = None;
        self.term
            .apply_to_rich_terms(|rt: &mut Self| rt.clean_pos());
    }

    pub fn app(rt1: RichTerm, rt2: RichTerm) -> RichTerm {
        Term::App(rt1, rt2).into()
    }

    pub fn var(s: String) -> RichTerm {
        Term::Var(Ident(s)).into()
    }

    pub fn fun(s: String, rt: RichTerm) -> RichTerm {
        Term::Fun(Ident(s), rt).into()
    }

    pub fn let_in(id: &str, e: RichTerm, t: RichTerm) -> RichTerm {
        Term::Let(Ident(id.to_string()), e, t).into()
    }

    pub fn ite(c: RichTerm, t: RichTerm, e: RichTerm) -> RichTerm {
        RichTerm::app(RichTerm::app(Term::Op1(UnaryOp::Ite(), c).into(), t), e)
    }

    pub fn plus(t0: RichTerm, t1: RichTerm) -> RichTerm {
        Term::Op2(BinaryOp::Plus(), t0, t1).into()
    }
}

impl From<RichTerm> for Term {
    fn from(rt: RichTerm) -> Self {
        *rt.term
    }
}

impl AsRef<Term> for RichTerm {
    fn as_ref(&self) -> &Term {
        &self.term
    }
}

impl From<Term> for RichTerm {
    fn from(t: Term) -> Self {
        Self::new(t)
    }
}
