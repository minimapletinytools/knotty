//! Preserve the selected ordered case arm as a retained runtime guard.
use super::*;
impl Compiler<'_, '_> {
    pub(super) fn case_body(
        &mut self,
        site: &Site,
        value: &Spanned<CExpr>,
        arms: &[(Spanned<CPattern>, Spanned<CExpr>)],
        request: &Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        let mut scrutinee = site.child(value);
        let scrutinee_name = format!("{forward}Scrutinee");
        let (ty, _) = self.forward(&scrutinee, &scrutinee_name)?;
        scrutinee.ty = Some(ty);
        let patterns = arms
            .iter()
            .map(|(p, _)| syntax::pattern(&p.node, &site.module))
            .collect::<Vec<_>>();
        let mut code = format!("case {scrutinee_name} {} of\n", self.current_value());
        for (index, (pattern, body)) in arms.iter().enumerate() {
            let scope = self.walker.scopes.len();
            self.walker
                .scopes
                .push(self.walker.scopes[site.scope].clone());
            self.walker.bind_pattern(
                &pattern.node,
                scrutinee.clone(),
                scope,
                &site.module,
                true,
            )?;
            let child = self.emit(
                Site {
                    scope,
                    ty: site.ty.clone(),
                    ..site.child(body)
                },
                request,
            )?;
            let guard = format!("{forward}Arm{index}");
            let choices = patterns
                .iter()
                .enumerate()
                .map(|(i, p)| format!("  {p} -> {}\n", if i == index { "True" } else { "False" }))
                .collect::<String>();
            self.declarations.push_str(&format!("{guard} :: {} -> Bool\n{guard} {} = case {scrutinee_name} {} of\n{choices}  _ -> False\n", self.input.source(), self.symbol, self.symbol));
            // Pattern bindings must not shadow the generated input/current/request
            // names. Match only in the guard and use wildcard bindings here.
            code.push_str(&format!("  {} -> do\n    (candidate, guard) <- {} input current request\n    pure (candidate, \\value -> {guard} value && guard value && fits request ({forward} value))\n", syntax::pattern(&erase_bindings(&pattern.node), &site.module), child.unravel));
        }
        code.push_str("  _ -> Nothing");
        Ok(code)
    }
}
fn erase_bindings(pattern: &CPattern) -> CPattern {
    match pattern {
        CPattern::Var(_) | CPattern::Wildcard(_) => CPattern::Wildcard(None),
        CPattern::As(p, _) => erase_bindings(&p.node),
        CPattern::Tuple(parts) => CPattern::Tuple(
            parts
                .iter()
                .map(|p| Spanned::new(p.span, erase_bindings(&p.node)))
                .collect(),
        ),
        CPattern::Ctor(name, parts) => CPattern::Ctor(
            name.clone(),
            parts
                .iter()
                .map(|p| Spanned::new(p.span, erase_bindings(&p.node)))
                .collect(),
        ),
        CPattern::Cons(a, b) => CPattern::Cons(
            Box::new(Spanned::new(a.span, erase_bindings(&a.node))),
            Box::new(Spanned::new(b.span, erase_bindings(&b.node))),
        ),
        // Record patterns bind names, so use an irrefutable wildcard in the
        // execution match. Their record type/fields were already checked.
        CPattern::Record(_) => CPattern::Wildcard(None),
        _ => pattern.clone(),
    }
}
