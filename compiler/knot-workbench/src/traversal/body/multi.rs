//! Joint callback solving. Pins and demand flags are internal; the public
//! companion returns one ordinary Maybe request for each curried argument.
use super::*;

pub(super) struct Inputs {
    pub types: Vec<Ty>,
    pub symbols: Vec<String>,
}
fn tuple_field(value: &str, arity: usize, index: usize) -> String {
    let names = (0..arity)
        .map(|i| format!("twinePart{i}"))
        .collect::<Vec<_>>();
    format!("(let {} = {value} in {})", group(&names), names[index])
}
impl Walker<'_> {
    pub(super) fn compile_multi_body(
        &mut self,
        callback: &Site,
        output_request: &Ty,
        ambient: &str,
    ) -> Result<GeneratedBody, EditError> {
        let forward = callback.ty.as_ref().unwrap().clone();
        let (types, _) = callback_shape(&forward)?;
        let count = types.len();
        let mut prefix = format!("twineBody{}x{}", self.visits, self.scopes.len());
        while self
            .snapshot
            .sources()
            .values()
            .any(|source| source.contains(&prefix))
        {
            prefix.push('x');
        }
        let symbol = format!("{prefix}Argument");
        let symbols = (0..count)
            .map(|i| format!("{prefix}Input{i}Root"))
            .collect::<Vec<_>>();
        let input = Ty::Record(BTreeMap::from([
            ("arguments".into(), Ty::Tuple(types.clone())),
            (
                "pins".into(),
                Ty::Tuple(types.iter().cloned().map(optional).collect()),
            ),
            (
                "requested".into(),
                Ty::Tuple(vec![Ty::Named("Bool".into(), vec![]); count]),
            ),
        ]));
        let callback = self.freeze(callback.clone())?;
        let span = callback.expr.span;
        let mut bindings = BTreeMap::from([("bodyCallback".into(), callback.clone())]);
        let mut application = Spanned::new(span, CExpr::Var(Ref::Local("bodyCallback".into())));
        for (i, ty) in types.iter().enumerate() {
            let name = format!("bodyArgument{i}");
            bindings.insert(
                name.clone(),
                Site {
                    expr: Spanned::new(span, CExpr::Var(Ref::TopLevel(symbols[i].clone()))),
                    module: vec![],
                    scope: 0,
                    ty: Some(ty.clone()),
                    fixed: false,
                    synthetic: true,
                    ..callback.clone()
                },
            );
            application = Spanned::new(
                span,
                CExpr::App(
                    Box::new(application),
                    Box::new(Spanned::new(span, CExpr::Var(Ref::Local(name)))),
                ),
            );
        }
        let scope = self.scopes.len();
        self.scopes.push(bindings);
        let application = Site {
            expr: application,
            scope,
            fixed: false,
            synthetic: true,
            ty: Some(callback_shape(&forward)?.1),
            ..callback
        };
        let mut compiler = Compiler {
            walker: self,
            input,
            ambient: ambient.into(),
            prefix,
            symbol,
            declarations: String::new(),
            nodes: 0,
            multi: Some(Inputs {
                types: types.clone(),
                symbols,
            }),
            dependencies: BTreeMap::new(),
        };
        let root = compiler.emit(application, output_request)?;
        // The solve stays in Maybe; the factory lifts it into a rule's answer.
        let outer = format!("{}Factory", compiler.prefix);
        let factory = format!("{outer}Solve");
        let feeds = (0..count).map(|i| format!("feed{i}")).collect::<Vec<_>>();
        let values = feeds
            .iter()
            .map(|feed| format!("{feed}.value"))
            .collect::<Vec<_>>();
        let pins = feeds
            .iter()
            .map(|feed| format!("{feed}.pinned"))
            .collect::<Vec<_>>();
        let requests = (0..count)
            .map(|i| format!("request{i}"))
            .collect::<Vec<_>>();
        let candidates = (0..count)
            .map(|i| format!("candidate{i}"))
            .collect::<Vec<_>>();
        let wanted = (0..count).map(|i| format!("wanted{i}")).collect::<Vec<_>>();
        let mut bindings = format!(
            "let {} = candidate.arguments\n    {} = candidate.requested\n",
            group(&candidates),
            group(&wanted)
        );
        for i in 0..count {
            bindings.push_str(&format!(
                "    request{i} = if wanted{i} then Just candidate{i} else Nothing\n"
            ));
        }
        let checks = (0..count)
            .map(|i| format!("pinAllows feed{i} request{i}"))
            .collect::<Vec<_>>()
            .join(" && ");
        let feed_types = Ty::Tuple(types.iter().map(candidates::feed_type).collect()).source();
        let request_types = Ty::Tuple(types.iter().cloned().map(optional).collect()).source();
        compiler.declarations.push_str(&format!(
            "{outer} :: ({}) -> {feed_types} -> {} -> Unravelled {request_types}\n{outer} actual feeds request = orSnag ({factory} actual feeds request)\n",
            forward.source(),
            output_request.source(),
        ));
        compiler.declarations.push_str(&format!(
            "{factory} :: ({}) -> {} -> {} -> Maybe {}\n{factory} actual {} request =\n  let original = {{ arguments = {}, pins = {}, requested = {} }}\n      input = {{ value = original, pinned = Nothing }}\n  in do\n    (candidate, guard) <- {} input original request\n{}    if guard candidate && fits request (actual {}) && {checks}\n    then Just {} else Nothing\n",
            forward.source(), Ty::Tuple(types.iter().map(candidates::feed_type).collect()).source(),
            output_request.source(), Ty::Tuple(types.into_iter().map(optional).collect()).source(),
            group(&feeds), group(&values), group(&pins), group(&vec!["False".into(); count]),
            root.unravel, indent(&indent(&bindings)), candidates.join(" "), group(&requests)
        ));
        compiler.walker.probes.declarations = ambient.into();
        compiler.walker.probes.check(&compiler.declarations)?;
        Ok(GeneratedBody {
            factory: outer,
            declarations: compiler.declarations,
        })
    }
}
impl Compiler<'_, '_> {
    pub(super) fn current_value(&self) -> &'static str {
        if self.multi.is_some() {
            "current"
        } else {
            "input.value"
        }
    }

    pub(super) fn bind_multi_roots(&self, source: &str) -> String {
        let Some(inputs) = &self.multi else {
            return source.into();
        };
        let bindings = inputs
            .symbols
            .iter()
            .enumerate()
            .map(|(i, name)| {
                format!(
                    "{name} = {}\n",
                    tuple_field(&format!("{}.arguments", self.symbol), inputs.types.len(), i)
                )
            })
            .collect::<String>();
        format!("let\n{}in\n{}", indent(&bindings), indent(source))
    }
    pub(super) fn multi_root(&self, site: &Site) -> Option<usize> {
        if !site.module.is_empty() {
            return None;
        }
        let CExpr::Var(Ref::TopLevel(name)) = &site.expr.node else {
            return None;
        };
        self.multi
            .as_ref()?
            .symbols
            .iter()
            .position(|symbol| symbol == name)
    }
    pub(super) fn input_pin(&self, forward: &str) -> String {
        let Some(inputs) = &self.multi else {
            return format!(
                "Prelude.maybe Nothing (\\_ -> Just ({forward} input.value)) input.pinned"
            );
        };
        let roots = &self.dependencies[forward];
        let checks = roots
            .iter()
            .map(|&i| {
                format!(
                    "Prelude.maybe False (\\_ -> True) {}",
                    tuple_field("input.value.pins", inputs.types.len(), i)
                )
            })
            .collect::<Vec<_>>();
        let condition = if checks.is_empty() {
            "True".into()
        } else {
            checks.join(" && ")
        };
        format!("if {condition} then Just ({forward} input.value) else Nothing")
    }
    pub(super) fn mark_multi_request(&self, root: Option<usize>, candidate: &str) -> String {
        let Some(root) = root else {
            return candidate.into();
        };
        let count = self.multi.as_ref().unwrap().types.len();
        let flags = (0..count)
            .map(|i| {
                if i == root {
                    "True".into()
                } else {
                    tuple_field(&format!("{candidate}.requested"), count, i)
                }
            })
            .collect::<Vec<_>>();
        format!("{{ {candidate} | requested = {} }}", group(&flags))
    }
}
