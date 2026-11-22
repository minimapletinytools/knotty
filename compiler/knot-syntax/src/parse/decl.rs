//! Declarations: signature+def merging into `FnDef`, `type`/`type alias`,
//! `instance` declarations, and the top-level declaration-list layout block
//! shared by both a bare fragment (most of the test corpus) and a full module
//! (which wraps this with a header and imports — see `module.rs`).

use crate::ast::decl::{Assoc, Decl, Fixity, FixityDecl, FnDef, InstanceDecl, InterfaceDecl};
use crate::ast::ty::Type;
use crate::ast::ty::TypeSignature;
use crate::error::{ErrorKind, ParseError};
use crate::span::{Span, Spanned};
use crate::state::ParseState;

/// A top-level item before it's known to be a definition or a bare
/// signature.
enum FnItem {
    Def(FnDef),
    Sig(String, Spanned<TypeSignature>),
}

/// Mirrors `Decl::TypeDecl`'s own field shape: name, type params, variants
/// (each a constructor name plus its argument types), and derived interfaces.
type TypeDeclParts = (String, Vec<String>, Vec<(String, Vec<Type>)>, Vec<String>);

impl<'a> ParseState<'a> {
    /// A declaration list with no module header/imports required — most of the
    /// test corpus is fragments at this level (M2–M4 grammar layers were tested
    /// before the module system existed), and `parse_module` builds on top of
    /// this for the real thing.
    pub fn parse_decls(&mut self) -> Result<Vec<Spanned<Decl>>, ParseError> {
        self.skip_trivia()?;
        if self.is_eof() {
            return Ok(Vec::new());
        }
        self.with_indent(|state| {
            let col = state.pos.col;
            let mut decls = vec![state.top_level_decl(col)?];
            loop {
                let checkpoint = state.clone();
                state.skip_trivia()?;
                if state.is_eof() {
                    break;
                }
                if state.check_aligned(col).is_err() {
                    *state = checkpoint;
                    break;
                }
                decls.push(state.top_level_decl(col)?);
            }
            Ok(decls)
        })
    }

    /// A leading `@name(args)`/`@{...}` (spec §13.1) attaches to `FnDef`'s own
    /// `annotations` field. `type`/`type alias`/`instance` declarations don't
    /// carry annotations in v0 (a resolved open question — possible V2
    /// feature), so one appearing there is a clear error rather than being
    /// silently accepted and dropped.
    pub(crate) fn top_level_decl(&mut self, col: u32) -> Result<Spanned<Decl>, ParseError> {
        let start = self.pos;
        self.skip_trivia()?;
        let annotations = if self.peek() == Some(b'@') {
            self.annotation_list()?
        } else {
            Vec::new()
        };
        self.skip_trivia()?;
        let decl = if self.peek_keyword("type") {
            self.type_or_alias_decl()?
        } else if self.peek_keyword("instance") {
            Decl::Instance(self.instance_decl()?)
        } else if self.peek_keyword("interface") {
            Decl::Interface(self.interface_decl()?)
        } else if ["infixl", "infixr", "infix"]
            .iter()
            .any(|k| self.peek_keyword(k))
        {
            Decl::Fixity(self.fixity_decl()?)
        } else {
            match self.fn_item(col)? {
                FnItem::Def(f) => Decl::Fn(f),
                FnItem::Sig(name, sig) => Decl::Primitive(name, sig),
            }
        };
        let decl = match decl {
            Decl::Fn(mut f) => {
                f.annotations = annotations;
                Decl::Fn(f)
            }
            other if annotations.is_empty() => other,
            _ => {
                return Err(ParseError::new(
                    ErrorKind::Custom(
                        "annotations are not supported on type/instance/fixity declarations in v0"
                            .to_string(),
                    ),
                    Span::new(start.offset, start.offset),
                )
                .fatal());
            }
        };
        Ok(Spanned::new(Span::new(start.offset, self.pos.offset), decl))
    }

    /// A signature (`name :: Type`) and its definition (`name arg = expr`) are
    /// two source lines but one `FnDef` — after an optional signature, this
    /// requires the *next* aligned item to share the same name and treats it as
    /// the definition, rather than leaving the outer declaration-list loop to
    /// (incorrectly) treat it as an unrelated sibling declaration.
    pub(crate) fn fn_decl(&mut self, col: u32) -> Result<FnDef, ParseError> {
        self.skip_trivia()?;
        let annotations = if self.peek() == Some(b'@') {
            self.annotation_list()?
        } else {
            Vec::new()
        };
        self.skip_trivia()?;
        match self.fn_item(col)? {
            FnItem::Def(mut f) => {
                f.annotations = annotations;
                Ok(f)
            }
            FnItem::Sig(name, _) => Err(ParseError::new(
                ErrorKind::Custom(format!(
                    "expected a definition for `{name}` after its signature"
                )),
                Span::new(self.pos.offset, self.pos.offset),
            )),
        }
    }

    /// A definition, optionally preceded by its signature, or a signature
    /// with no definition after it (a primitive, or an interface method
    /// without a default). Whether a bare signature is allowed is up to the
    /// caller and `validate`.
    fn fn_item(&mut self, col: u32) -> Result<FnItem, ParseError> {
        let name = self.decl_name()?.node;
        self.skip_trivia()?;

        let signature = if self.peek() == Some(b':') && self.peek_at(1) == Some(b':') {
            let sig_start = self.pos;
            self.bump();
            self.bump();
            self.skip_trivia()?;
            let sig = self.type_signature()?;
            let sig_span = Span::new(sig_start.offset, self.pos.offset);
            let sig = Spanned::new(sig_span, sig);
            let after_sig = self.clone();
            self.skip_trivia()?;
            let same_name_follows = !self.is_eof()
                && self.check_aligned(col).is_ok()
                && self
                    .try_parse(Self::decl_name)
                    .is_ok_and(|n| n.node == name);
            if !same_name_follows {
                *self = after_sig;
                return Ok(FnItem::Sig(name, sig));
            }
            Some(sig)
        } else {
            None
        };

        let mut params = Vec::new();
        loop {
            let checkpoint = self.clone();
            self.skip_trivia()?;
            if self.peek() == Some(b'=') || !self.continues_layout() {
                *self = checkpoint;
                break;
            }
            match self.pattern_atom() {
                Ok(p) => params.push(p),
                Err(err) if err.fatal => return Err(err),
                Err(_) => {
                    *self = checkpoint;
                    break;
                }
            }
        }
        self.expect_byte(b'=')?;
        self.skip_trivia()?;
        let body = self.expr()?;
        Ok(FnItem::Def(FnDef {
            name,
            signature,
            params,
            body,
            annotations: Vec::new(),
        }))
    }

    /// `interface [Context =>] Name var where` followed by an indented block
    /// of method signatures and default definitions (spec §10.2).
    fn interface_decl(&mut self) -> Result<InterfaceDecl, ParseError> {
        self.expect_keyword("interface")?;
        self.skip_trivia()?;
        let superclasses = self
            .try_parse(Self::constraint_list_arrow)
            .unwrap_or_default();
        let name = self.upper_ident_segment()?.node;
        self.skip_trivia()?;
        let mut vars = vec![self.lower_ident()?.node];
        self.skip_trivia()?;
        while !self.peek_keyword("where") {
            vars.push(self.lower_ident()?.node);
            self.skip_trivia()?;
        }
        self.expect_keyword("where")?;
        let mut decl = InterfaceDecl {
            name,
            vars,
            superclasses,
            methods: Vec::new(),
            defaults: Vec::new(),
        };
        self.skip_trivia()?;
        if self.is_eof() || !self.continues_layout() {
            return Ok(decl);
        }
        self.with_indent(|state| {
            let col = state.pos.col;
            loop {
                match state.fn_item(col)? {
                    FnItem::Sig(name, sig) => decl.methods.push((name, sig)),
                    FnItem::Def(mut f) => {
                        if let Some(sig) = f.signature.take() {
                            decl.methods.push((f.name.clone(), sig));
                        }
                        decl.defaults.push(f);
                    }
                }
                let checkpoint = state.clone();
                state.skip_trivia()?;
                if state.is_eof() || state.check_aligned(col).is_err() {
                    *state = checkpoint;
                    break;
                }
            }
            Ok(())
        })?;
        Ok(decl)
    }

    /// A declaration's name: an ordinary `lower_ident`, or an operator symbol
    /// wrapped in parens — an interface method (`(==)`) or a user-defined
    /// operator (`(+++)`).
    pub(crate) fn decl_name(&mut self) -> Result<Spanned<String>, ParseError> {
        self.skip_trivia()?;
        if self.peek() != Some(b'(') {
            return self.lower_ident();
        }
        let start = self.pos;
        self.bump(); // '('
        let Some(symbol) = self.peek_operator_symbol() else {
            return Err(ParseError::new(
                ErrorKind::Expected("an operator symbol"),
                Span::new(self.pos.offset, self.pos.offset),
            ));
        };
        for _ in 0..symbol.len() {
            self.bump();
        }
        self.expect_byte(b')')?;
        Ok(Spanned::new(
            Span::new(start.offset, self.pos.offset),
            symbol.to_string(),
        ))
    }

    /// `infixl 6 (+++)`, `` infixr 5 (:), `cons` ``, `infix 4 (~=)`: an
    /// associativity keyword, a precedence 0-9, then one or more operators,
    /// each a parenthesized symbol or a backticked function name (spec §7.4).
    pub fn fixity_decl(&mut self) -> Result<FixityDecl, ParseError> {
        self.skip_trivia()?;
        let assoc = if self.peek_keyword("infixl") {
            self.expect_keyword("infixl")?;
            Assoc::Left
        } else if self.peek_keyword("infixr") {
            self.expect_keyword("infixr")?;
            Assoc::Right
        } else {
            self.expect_keyword("infix")?;
            Assoc::None
        };
        self.skip_trivia()?;
        let precedence = match (self.peek(), self.peek_at(1)) {
            (Some(d), next)
                if d.is_ascii_digit() && !next.is_some_and(|b| b.is_ascii_alphanumeric()) =>
            {
                self.bump();
                d - b'0'
            }
            _ => {
                return Err(ParseError::new(
                    ErrorKind::Expected("a precedence from 0 to 9"),
                    Span::new(self.pos.offset, self.pos.offset),
                )
                .fatal())
            }
        };
        let mut operators = vec![self.fixity_operator()?];
        loop {
            let checkpoint = self.clone();
            self.skip_trivia()?;
            if self.peek() != Some(b',') {
                *self = checkpoint;
                break;
            }
            self.bump();
            operators.push(self.fixity_operator()?);
        }
        Ok(FixityDecl {
            fixity: Fixity { assoc, precedence },
            operators,
        })
    }

    /// One operator in a fixity declaration: `(+++)` or `` `div` ``.
    fn fixity_operator(&mut self) -> Result<Spanned<String>, ParseError> {
        self.skip_trivia()?;
        if self.peek() == Some(b'`') {
            let start = self.pos;
            self.bump();
            let name = self.lower_ident()?.node;
            self.expect_byte(b'`')?;
            return Ok(Spanned::new(Span::new(start.offset, self.pos.offset), name));
        }
        if self.peek() != Some(b'(') {
            return Err(ParseError::new(
                ErrorKind::Expected("an operator like `(+++)` or `` `name` ``"),
                Span::new(self.pos.offset, self.pos.offset),
            )
            .fatal());
        }
        self.decl_name()
    }

    fn type_or_alias_decl(&mut self) -> Result<Decl, ParseError> {
        let checkpoint = self.clone();
        self.expect_keyword("type")?;
        self.skip_trivia()?;
        let is_alias = self.peek_keyword("alias");
        *self = checkpoint;
        if is_alias {
            let (name, params, ty) = self.type_alias_decl()?;
            Ok(Decl::TypeAlias(name, params, ty))
        } else {
            let (name, params, variants, deriving) = self.type_decl()?;
            Ok(Decl::TypeDecl(name, params, variants, deriving))
        }
    }

    fn type_params(&mut self) -> Result<Vec<String>, ParseError> {
        let mut params = Vec::new();
        loop {
            let checkpoint = self.clone();
            self.skip_trivia()?;
            // An opaque `type Int` ends at the line; the next declaration
            // starts back at the block's column.
            if !self.starts_value_ident() || !self.continues_layout() {
                *self = checkpoint;
                break;
            }
            params.push(self.lower_ident()?.node);
        }
        Ok(params)
    }

    fn type_alias_decl(&mut self) -> Result<(String, Vec<String>, Type), ParseError> {
        self.expect_keyword("type")?;
        self.skip_trivia()?;
        self.expect_keyword("alias")?;
        self.skip_trivia()?;
        let name = self.upper_ident_segment()?.node;
        let params = self.type_params()?;
        self.expect_byte(b'=')?;
        self.skip_trivia()?;
        let ty = self.type_arrow()?;
        Ok((name, params, ty))
    }

    fn type_decl(&mut self) -> Result<TypeDeclParts, ParseError> {
        self.expect_keyword("type")?;
        self.skip_trivia()?;
        let name = self.upper_ident_segment()?.node;
        let params = self.type_params()?;
        // `type Int`: an opaque type with no constructors, provided by the
        // runtime. Only a library module may declare one (see `validate`).
        let checkpoint = self.clone();
        self.skip_trivia()?;
        if self.peek() != Some(b'=') {
            *self = checkpoint;
            return Ok((name, params, Vec::new(), Vec::new()));
        }
        self.expect_byte(b'=')?;
        self.skip_trivia()?;
        let mut variants = vec![self.type_variant()?];
        loop {
            let checkpoint = self.clone();
            self.skip_trivia()?;
            if self.peek() != Some(b'|') {
                *self = checkpoint;
                break;
            }
            self.bump();
            self.skip_trivia()?;
            variants.push(self.type_variant()?);
        }
        let checkpoint = self.clone();
        self.skip_trivia()?;
        let deriving = if self.peek_keyword("deriving") {
            self.deriving_clause()?
        } else {
            *self = checkpoint;
            Vec::new()
        };
        Ok((name, params, variants, deriving))
    }

    /// `deriving (Eq, Ord, Show)` or the single-name form `deriving Eq`, with
    /// the `deriving` keyword itself not yet consumed. Which interfaces can
    /// actually be derived (and whether a given ADT's own shape qualifies)
    /// is a semantic question for `knot-checker`, not this layer -- this
    /// only ever collects whatever uppercase names appear here, exactly
    /// like `single_constraint`'s own interface name doesn't get checked
    /// against the closed set until canonicalization either.
    fn deriving_clause(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect_keyword("deriving")?;
        self.skip_trivia()?;
        if self.peek() == Some(b'(') {
            self.bump();
            self.skip_trivia()?;
            let mut names = vec![self.upper_ident_segment()?.node];
            self.skip_trivia()?;
            while self.peek() == Some(b',') {
                self.bump();
                self.skip_trivia()?;
                names.push(self.upper_ident_segment()?.node);
                self.skip_trivia()?;
            }
            self.expect_byte(b')')?;
            Ok(names)
        } else {
            Ok(vec![self.upper_ident_segment()?.node])
        }
    }

    fn type_variant(&mut self) -> Result<(String, Vec<Type>), ParseError> {
        let ctor = self.upper_ident_segment()?.node;
        let mut args = Vec::new();
        loop {
            let checkpoint = self.clone();
            self.skip_trivia()?;
            if !self.continues_layout() {
                *self = checkpoint;
                break;
            }
            match self.type_atom() {
                Ok(ty) => args.push(ty),
                Err(err) if err.fatal => return Err(err),
                Err(_) => {
                    *self = checkpoint;
                    break;
                }
            }
        }
        Ok((ctor, args))
    }

    /// Haskell-style: `instance Eq Shape where (==) a b = ...`. Methods reuse
    /// `fn_decl` directly — since an instance method never repeats its own `::`
    /// signature (it's inherited from the interface), `fn_decl` naturally just
    /// takes the "no signature" branch for each one.
    fn instance_decl(&mut self) -> Result<InstanceDecl, ParseError> {
        self.expect_keyword("instance")?;
        self.skip_trivia()?;
        let constraints = self
            .try_parse(Self::constraint_list_arrow)
            .unwrap_or_default();
        let interface = self.upper_ident_segment()?.node;
        self.skip_trivia()?;
        let mut targets = vec![self.type_atom()?];
        self.skip_trivia()?;
        while !self.peek_keyword("where") {
            targets.push(self.type_atom()?);
            self.skip_trivia()?;
        }
        self.expect_keyword("where")?;
        // An empty body (nothing indented under `where`) parses fine, so a
        // missing method is reported by the checker as one, not as a parse
        // error.
        let checkpoint = self.clone();
        self.skip_trivia()?;
        if self.is_eof() || !self.continues_layout() {
            *self = checkpoint;
            return Ok(InstanceDecl {
                interface,
                constraints,
                targets,
                methods: Vec::new(),
            });
        }
        let methods = self.with_indent(|state| {
            let col = state.pos.col;
            let mut methods = vec![state.fn_decl(col)?];
            loop {
                let checkpoint = state.clone();
                state.skip_trivia()?;
                if state.check_aligned(col).is_err() {
                    *state = checkpoint;
                    break;
                }
                methods.push(state.fn_decl(col)?);
            }
            Ok(methods)
        })?;
        Ok(InstanceDecl {
            interface,
            constraints,
            targets,
            methods,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::expr::Expr;
    use crate::ast::pattern::Pattern;
    use crate::ast::ty::Type;

    fn decls(src: &str) -> Vec<Decl> {
        let mut s = ParseState::new(src);
        let result = s.parse_decls().unwrap();
        assert!(
            s.is_eof(),
            "leftover input: {:?}",
            s.text_since(s.pos.offset)
        );
        result.into_iter().map(|d| d.node).collect()
    }

    #[test]
    fn fn_def_without_signature() {
        let ds = decls("myFunc x = x + 1");
        let [Decl::Fn(f)] = ds.as_slice() else {
            panic!("expected one Fn decl")
        };
        assert_eq!(f.name, "myFunc");
        assert!(f.signature.is_none());
        assert_eq!(f.params.len(), 1);
        assert!(matches!(f.body.node, Expr::OpChain(_, _)));
    }

    #[test]
    fn fn_def_with_signature_merges_into_one_decl() {
        let ds = decls("myFunc :: Int -> Int\nmyFunc x = x + 1");
        assert_eq!(ds.len(), 1);
        let [Decl::Fn(f)] = ds.as_slice() else {
            panic!("expected one merged Fn decl")
        };
        assert_eq!(f.name, "myFunc");
        assert!(f.signature.is_some());
        assert_eq!(f.params.len(), 1);
    }

    #[test]
    fn fn_def_with_constrained_signature() {
        let ds = decls("myMax :: Ord a => a -> a -> a\nmyMax x y = if x > y then x else y");
        let [Decl::Fn(f)] = ds.as_slice() else {
            panic!("expected one Fn decl")
        };
        let sig = f.signature.as_ref().unwrap();
        assert_eq!(sig.node.constraints.len(), 1);
    }

    #[test]
    fn signature_without_matching_definition_errors() {
        // It parses as a primitive, which only a library module may declare.
        let src = "module M exposing (..)\nmyFunc :: Int -> Int\nother x = x";
        assert!(crate::parse(src).is_err());
        let m = crate::parse_library(src).unwrap();
        assert!(matches!(&m.decls[0].node, Decl::Primitive(n, _) if n == "myFunc"));
    }

    #[test]
    fn interface_with_signatures_and_defaults() {
        let src = "module Prelude exposing (..)\n\
                   interface Eq a => Ord a where\n  \
                   compare :: a -> a -> Ordering\n  \
                   (<) :: a -> a -> Bool\n  \
                   (<) x y = True\n  \
                   max :: a -> a -> a\n\
                   type Int\n\
                   x = 1";
        let m = crate::parse_library(src).unwrap();
        let Decl::Interface(i) = &m.decls[0].node else {
            panic!("expected an interface, got {:?}", m.decls[0].node)
        };
        assert_eq!((i.name.as_str(), i.vars[0].as_str()), ("Ord", "a"));
        assert_eq!(i.superclasses.len(), 1);
        let methods: Vec<&str> = i.methods.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(methods, ["compare", "<", "max"]);
        assert_eq!(i.defaults.len(), 1);
        assert!(i.defaults[0].signature.is_none());
        assert!(
            matches!(&m.decls[1].node, Decl::TypeDecl(n, _, v, _) if n == "Int" && v.is_empty())
        );
        assert!(crate::parse(src).is_err());
    }

    #[test]
    fn multiple_top_level_decls() {
        let ds = decls("x = 1\ny = 2");
        assert_eq!(ds.len(), 2);
    }

    #[test]
    fn fn_params_do_not_swallow_the_next_sibling_declaration() {
        // Regression guard for exactly the bug `continues_layout` prevents in
        // `fn_decl`'s params loop: without it, a malformed/missing `=` could
        // greedily consume the next declaration's name as a parameter.
        let mut s = ParseState::new("x\ny = 2");
        assert!(s.parse_decls().is_err());
    }

    #[test]
    fn type_alias_declaration() {
        let ds = decls("type alias Point = { x : Float, y : Float }");
        let [Decl::TypeAlias(name, params, ty)] = ds.as_slice() else {
            panic!("expected TypeAlias")
        };
        assert_eq!(name, "Point");
        assert!(params.is_empty());
        assert!(matches!(ty, Type::Record(_, None)));
    }

    #[test]
    fn adt_multi_variant_declaration() {
        let src = "type Shape\n  = Circle Float\n  | Rectangle Float Float\n  | Triangle Float Float Float";
        let ds = decls(src);
        let [Decl::TypeDecl(name, params, variants, _deriving)] = ds.as_slice() else {
            panic!("expected TypeDecl")
        };
        assert_eq!(name, "Shape");
        assert!(params.is_empty());
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].0, "Circle");
        assert_eq!(variants[0].1.len(), 1);
        assert_eq!(variants[2].1.len(), 3);
    }

    #[test]
    fn a_user_type_can_reuse_true_and_false_as_its_own_constructor_names() {
        // True/False are prelude constructors, not reserved words -- removed
        // from RESERVED_WORDS since they were dead entries there anyway
        // (upper_ident_segment, which parses constructor names, never
        // consulted that lowercase-only list in the first place).
        let ds = decls("type Toggle\n  = True\n  | False");
        let [Decl::TypeDecl(name, params, variants, _deriving)] = ds.as_slice() else {
            panic!("expected TypeDecl")
        };
        assert_eq!(name, "Toggle");
        assert!(params.is_empty());
        assert_eq!(variants[0].0, "True");
        assert_eq!(variants[1].0, "False");
    }

    #[test]
    fn adt_with_no_deriving_clause_has_an_empty_list() {
        let ds = decls("type Shape\n  = Circle Float");
        let [Decl::TypeDecl(_, _, _, deriving)] = ds.as_slice() else {
            panic!("expected TypeDecl")
        };
        assert!(deriving.is_empty());
    }

    #[test]
    fn adt_deriving_a_single_interface_needs_no_parens() {
        let ds = decls("type Shape\n  = Circle Float\n  deriving Eq");
        let [Decl::TypeDecl(_, _, _, deriving)] = ds.as_slice() else {
            panic!("expected TypeDecl")
        };
        assert_eq!(deriving, &vec!["Eq".to_string()]);
    }

    #[test]
    fn adt_deriving_multiple_interfaces_needs_parens() {
        let ds = decls("type Shape\n  = Circle Float\n  deriving (Eq, Ord, Show)");
        let [Decl::TypeDecl(_, _, _, deriving)] = ds.as_slice() else {
            panic!("expected TypeDecl")
        };
        assert_eq!(
            deriving,
            &vec!["Eq".to_string(), "Ord".to_string(), "Show".to_string()]
        );
    }

    #[test]
    fn generic_adt_declaration() {
        let ds = decls("type Option a\n  = Some a\n  | None");
        let [Decl::TypeDecl(name, params, variants, _deriving)] = ds.as_slice() else {
            panic!("expected TypeDecl")
        };
        assert_eq!(name, "Option");
        assert_eq!(params, &vec!["a".to_string()]);
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1].1.len(), 0);
    }

    #[test]
    fn instance_declaration() {
        let src = "instance Eq Shape where\n  (==) a b = True";
        let ds = decls(src);
        let [Decl::Instance(inst)] = ds.as_slice() else {
            panic!("expected Instance")
        };
        assert_eq!(inst.interface, "Eq");
        assert!(inst.constraints.is_empty());
        assert_eq!(
            inst.targets[0],
            Type::Named("Shape".to_string(), Vec::new())
        );
        assert_eq!(inst.methods.len(), 1);
        assert_eq!(inst.methods[0].name, "==");
    }

    #[test]
    fn instance_with_an_empty_body_parses() {
        let ds = decls("instance Ord Shape where\nf x = x");
        let Decl::Instance(inst) = &ds[0] else {
            panic!("expected an instance, got {:?}", ds[0]);
        };
        assert!(inst.methods.is_empty());
        assert!(matches!(&ds[1], Decl::Fn(f) if f.name == "f"));
        let Decl::Instance(inst) = &decls("instance Ord Shape where")[0] else {
            panic!("expected an instance");
        };
        assert!(inst.methods.is_empty());
    }

    #[test]
    fn instance_declaration_with_constraint_and_parenthesized_target() {
        let src = "instance Eq a => Eq (Maybe a) where\n  (==) a b = True";
        let ds = decls(src);
        let [Decl::Instance(inst)] = ds.as_slice() else {
            panic!("expected Instance")
        };
        assert_eq!(inst.constraints.len(), 1);
        assert_eq!(
            inst.targets[0],
            Type::Named("Maybe".to_string(), vec![Type::Var("a".to_string())])
        );
    }

    #[test]
    fn instance_method_reuses_pattern_grammar_for_params() {
        let src = "instance Eq Shape where\n  (==) a b =\n    case (a, b) of\n      (x, y) -> True";
        let ds = decls(src);
        let [Decl::Instance(inst)] = ds.as_slice() else {
            panic!("expected Instance")
        };
        assert!(matches!(inst.methods[0].params[0].node, Pattern::Var(_)));
        assert!(matches!(inst.methods[0].body.node, Expr::Case(_, _)));
    }

    #[test]
    fn stacked_annotations_attach_to_fn_def() {
        let src = "@nodeId(\"f1\")\n@position(100, 200)\nmyFunc :: Int -> Int\nmyFunc x = x + 1";
        let ds = decls(src);
        let [Decl::Fn(f)] = ds.as_slice() else {
            panic!("expected one Fn decl")
        };
        assert_eq!(f.name, "myFunc");
        assert_eq!(f.annotations.len(), 2);
        assert_eq!(f.annotations[0].key, "nodeId");
        assert!(f.signature.is_some());
    }

    #[test]
    fn block_annotation_attaches_to_fn_def_without_signature() {
        let src = "@{ nodeId = \"f1\", color = \"blue\" }\nmyFunc x = x + 1";
        let ds = decls(src);
        let [Decl::Fn(f)] = ds.as_slice() else {
            panic!("expected one Fn decl")
        };
        assert_eq!(f.annotations.len(), 2);
    }

    #[test]
    fn annotation_on_type_alias_is_rejected() {
        let src = "@doc(\"a point\")\ntype alias Point = { x : Float, y : Float }";
        let mut s = ParseState::new(src);
        assert!(s.parse_decls().is_err());
    }

    #[test]
    fn annotation_on_instance_is_rejected() {
        let src = "@doc(\"eq for shapes\")\ninstance Eq Shape where\n  (==) a b = True";
        let mut s = ParseState::new(src);
        assert!(s.parse_decls().is_err());
    }

    #[test]
    fn multiple_annotated_declarations_in_sequence() {
        // Regression guard: one declaration's annotation list must not bleed
        // into the next sibling declaration's.
        let src = "@nodeId(\"f1\")\nfirst x = x\n\n@nodeId(\"f2\")\nsecond y = y";
        let ds = decls(src);
        assert_eq!(ds.len(), 2);
        let [Decl::Fn(a), Decl::Fn(b)] = ds.as_slice() else {
            panic!("expected two Fn decls")
        };
        assert_eq!(a.annotations[0].key, "nodeId");
        assert_eq!(b.annotations[0].key, "nodeId");
        assert_ne!(a.annotations[0].value.node, b.annotations[0].value.node);
    }

    #[test]
    fn fixity_declarations() {
        use crate::ast::decl::{Assoc, Fixity};
        let ds = decls("infixl 6 (+++)\ninfixr 5 (<:), `cons`\ninfix 4 (~=)");
        let fixities: Vec<(Fixity, Vec<String>)> = ds
            .iter()
            .map(|d| match d {
                Decl::Fixity(f) => (
                    f.fixity,
                    f.operators.iter().map(|o| o.node.clone()).collect(),
                ),
                other => panic!("expected a fixity declaration, got {other:?}"),
            })
            .collect();
        assert_eq!(
            fixities,
            vec![
                (
                    Fixity {
                        assoc: Assoc::Left,
                        precedence: 6
                    },
                    vec!["+++".to_string()]
                ),
                (
                    Fixity {
                        assoc: Assoc::Right,
                        precedence: 5
                    },
                    vec!["<:".to_string(), "cons".to_string()]
                ),
                (
                    Fixity {
                        assoc: Assoc::None,
                        precedence: 4
                    },
                    vec!["~=".to_string()]
                ),
            ]
        );
    }

    #[test]
    fn fixity_declaration_needs_a_single_digit_precedence() {
        for src in ["infixl 10 (+++)", "infixl (+++)", "infixl 6 +++"] {
            let mut s = ParseState::new(src);
            assert!(s.parse_decls().is_err(), "{src}");
        }
    }

    #[test]
    fn operator_definition_with_a_signature() {
        let ds = decls("(<+>) :: Int -> Int -> Int\n(<+>) a b = a");
        let Decl::Fn(f) = &ds[0] else {
            panic!("expected Fn")
        };
        assert_eq!(f.name, "<+>");
        assert!(f.signature.is_some());
    }
}
