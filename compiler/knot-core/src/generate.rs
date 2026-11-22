//! Generated dictionaries are ordinary core functions. Field evidence comes
//! from the checker; neither this module nor Twine selects nominal instances.
use crate::{lower::Lower, *};
use knot_canonical::ast::CDecl;
use knot_checker::program::TypedInstance;

#[derive(Clone)]
enum Product {
    Constructor(GlobalName, usize),
    Tuple(usize),
    Record(Vec<String>),
    Unit,
}
impl Product {
    fn arity(&self) -> usize {
        match self {
            Self::Constructor(_, n) | Self::Tuple(n) => *n,
            Self::Record(names) => names.len(),
            Self::Unit => 0,
        }
    }
    fn pattern(&self, fields: &[LocalId]) -> Pattern {
        match self {
            Self::Constructor(name, _) => Pattern::Ctor(
                name.clone(),
                fields.iter().copied().map(Pattern::Bind).collect(),
            ),
            Self::Tuple(_) => Pattern::Tuple(fields.iter().copied().map(Pattern::Bind).collect()),
            Self::Record(names) => {
                Pattern::Record(names.iter().cloned().zip(fields.iter().copied()).collect())
            }
            Self::Unit => Pattern::Lit(Literal::Unit),
        }
    }
}
impl Lower<'_> {
    fn local_expr(&mut self, id: LocalId, o: Origin) -> ExprId {
        self.add(o, Kind::Local(id))
    }
    fn tag(&mut self, name: &str, o: Origin) -> ExprId {
        self.add(o, Kind::Ctor(prelude(name), 0))
    }
    fn text(&mut self, text: impl Into<String>, o: Origin) -> ExprId {
        self.add(o, Kind::Lit(Literal::String(text.into())))
    }
    fn method(
        &mut self,
        dict: ExprId,
        interface: &str,
        name: &str,
        args: Vec<ExprId>,
        o: Origin,
    ) -> ExprId {
        let method = self.add(
            o,
            Kind::Select(dict, Field::Method(interface.into(), name.into())),
        );
        self.app(o, method, args)
    }
    fn primitive(&mut self, name: &str, args: Vec<ExprId>, o: Origin) -> ExprId {
        let f = self.add(o, Kind::Prim(name.into()));
        self.app(o, f, args)
    }
    fn concat(&mut self, parts: Vec<ExprId>, o: Origin) -> ExprId {
        let mut parts = parts.into_iter();
        let Some(mut text) = parts.next() else {
            return self.text("", o);
        };
        for part in parts {
            text = self.primitive("primStringAppend", vec![text, part], o);
        }
        text
    }
    fn product_value(&mut self, product: &Product, fields: Vec<ExprId>, o: Origin) -> ExprId {
        match product {
            Product::Constructor(name, n) => {
                let ctor = self.add(o, Kind::Ctor(name.clone(), *n));
                self.app(o, ctor, fields)
            }
            Product::Tuple(_) => self.add(o, Kind::Tuple(fields)),
            Product::Record(names) => {
                self.add(o, Kind::Record(names.iter().cloned().zip(fields).collect()))
            }
            Product::Unit => self.add(o, Kind::Lit(Literal::Unit)),
        }
    }
    fn fresh_fields(&mut self, product: &Product) -> Vec<LocalId> {
        (0..product.arity()).map(|_| self.fresh()).collect()
    }
    /// Compare/validate until the first failure, in declaration/label order.
    fn compare_fields(
        &mut self,
        interface: &str,
        dicts: &[ExprId],
        left: &[LocalId],
        right: &[LocalId],
        o: Origin,
    ) -> ExprId {
        let (method, equal, failures): (_, _, &[_]) = match interface {
            "Eq" => ("==", "True", &["False"]),
            "Constriction" => ("fits", "True", &["False"]),
            "Ord" => ("compare", "EQ", &["LT", "GT"]),
            _ => unreachable!("unsupported product comparison"),
        };
        let mut result = self.tag(equal, o);
        for ((dict, left), right) in dicts.iter().zip(left).zip(right).rev() {
            let a = self.local_expr(*left, o);
            let b = self.local_expr(*right, o);
            let cmp = self.method(*dict, interface, method, vec![a, b], o);
            let mut arms = vec![(Pattern::Ctor(prelude(equal), vec![]), result)];
            for name in failures {
                let value = self.tag(name, o);
                arms.push((Pattern::Ctor(prelude(name), vec![]), value));
            }
            result = self.add(o, Kind::Case(cmp, arms));
        }
        result
    }
    fn show_product(
        &mut self,
        product: &Product,
        dicts: &[ExprId],
        fields: &[LocalId],
        precedence: LocalId,
        o: Origin,
    ) -> ExprId {
        let (opening, closing, separator, child_precedence) = match product {
            Product::Constructor(name, _) => (name.name.as_str(), "", " ", 11),
            Product::Tuple(_) => ("(", ")", ",", 0),
            Product::Record(names) if names.is_empty() => return self.text("{}", o),
            Product::Record(_) => ("{", " }", ", ", 0),
            Product::Unit => return self.text("()", o),
        };
        let mut parts = vec![self.text(opening, o)];
        for (i, (dict, field)) in dicts.iter().zip(fields).enumerate() {
            match product {
                Product::Constructor(..) => parts.push(self.text(" ", o)),
                Product::Record(names) => parts.push(self.text(
                    format!("{}{} = ", if i == 0 { " " } else { separator }, names[i]),
                    o,
                )),
                _ if i > 0 => parts.push(self.text(separator, o)),
                _ => {}
            }
            let d = self.add(o, Kind::Lit(Literal::Int(child_precedence)));
            let field = self.local_expr(*field, o);
            parts.push(self.method(*dict, "Show", "showsPrec", vec![d, field], o));
        }
        if !closing.is_empty() {
            parts.push(self.text(closing, o));
        }
        let text = self.concat(parts, o);
        if matches!(product, Product::Constructor(_, n) if *n > 0) {
            // Share the body when adding parentheses, and do not evaluate its
            // fields merely to decide whether parentheses are required.
            let shared = self.fresh();
            let open = self.text("(", o);
            let body = self.local_expr(shared, o);
            let close = self.text(")", o);
            let wrapped = self.concat(vec![open, body, close], o);
            let body = self.local_expr(shared, o);
            let d = self.local_expr(precedence, o);
            let ten = self.add(o, Kind::Lit(Literal::Int(10)));
            let cmp = self.primitive("primIntCompare", vec![d, ten], o);
            let choice = self.add(
                o,
                Kind::Case(
                    cmp,
                    vec![
                        (Pattern::Ctor(prelude("GT"), vec![]), wrapped),
                        (Pattern::Wildcard, body),
                    ],
                ),
            );
            self.add(o, Kind::Let(vec![(shared, text)], choice))
        } else {
            text
        }
    }
    /// Build required methods; the recursive dictionary installs defaults and
    /// superclasses separately. Products are in constructor declaration order.
    fn generated_method(
        &mut self,
        products: &[Product],
        dictionaries: &[Vec<ExprId>],
        name: &str,
        o: Origin,
    ) -> ExprId {
        if name == "materialize" {
            let product = &products[0];
            let request = self.fresh();
            let inputs = self.fresh_fields(product);
            let outputs = self.fresh_fields(product);
            let fields = outputs.iter().map(|id| self.local_expr(*id, o)).collect();
            let value = self.product_value(product, fields, o);
            let just = self.add(o, Kind::Ctor(prelude("Just"), 1));
            let mut body = self.app(o, just, vec![value]);
            for ((dict, input), output) in dictionaries[0].iter().zip(&inputs).zip(&outputs).rev() {
                let input = self.local_expr(*input, o);
                let candidate = self.method(*dict, "Constriction", "materialize", vec![input], o);
                let nothing = self.tag("Nothing", o);
                body = self.add(
                    o,
                    Kind::Case(
                        candidate,
                        vec![
                            (
                                Pattern::Ctor(prelude("Just"), vec![Pattern::Bind(*output)]),
                                body,
                            ),
                            (Pattern::Ctor(prelude("Nothing"), vec![]), nothing),
                        ],
                    ),
                );
            }
            let value = self.local_expr(request, o);
            let body = self.add(o, Kind::Case(value, vec![(product.pattern(&inputs), body)]));
            return self.lambdas(o, vec![request], body);
        }
        if name == "empty" {
            let fields = dictionaries[0]
                .iter()
                .map(|d| self.method(*d, "Monoid", "empty", vec![], o))
                .collect();
            return self.product_value(&products[0], fields, o);
        }
        let x = self.fresh();
        let y = self.fresh();
        // Share one rank expression across the fallback arms. This keeps
        // generated code linear in the constructor/field count.
        let rank_y = if name == "compare" && products.len() > 1 {
            let value = self.local_expr(y, o);
            let arms = products
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let fields = self.fresh_fields(p);
                    let rank = self.add(o, Kind::Lit(Literal::Int(i as i64)));
                    (p.pattern(&fields), rank)
                })
                .collect();
            Some(self.add(o, Kind::Case(value, arms)))
        } else {
            None
        };
        let mut outer = Vec::new();
        for (i, product) in products.iter().enumerate() {
            let left = self.fresh_fields(product);
            let body = if name == "showsPrec" {
                self.show_product(product, &dictionaries[i], &left, x, o)
            } else {
                let right = self.fresh_fields(product);
                let value = if name == "<>" {
                    let fields = dictionaries[i]
                        .iter()
                        .zip(&left)
                        .zip(&right)
                        .map(|((d, l), r)| {
                            let a = self.local_expr(*l, o);
                            let b = self.local_expr(*r, o);
                            self.method(*d, "Semigroup", "<>", vec![a, b], o)
                        })
                        .collect();
                    self.product_value(product, fields, o)
                } else {
                    self.compare_fields(
                        match name {
                            "==" => "Eq",
                            "fits" => "Constriction",
                            _ => "Ord",
                        },
                        &dictionaries[i],
                        &left,
                        &right,
                        o,
                    )
                };
                let mut inner = vec![(product.pattern(&right), value)];
                if products.len() > 1 {
                    let mismatch = if name == "==" {
                        self.tag("False", o)
                    } else {
                        let rank = self.add(o, Kind::Lit(Literal::Int(i as i64)));
                        self.primitive("primIntCompare", vec![rank, rank_y.unwrap()], o)
                    };
                    inner.push((Pattern::Wildcard, mismatch));
                }
                let value = self.local_expr(y, o);
                self.add(o, Kind::Case(value, inner))
            };
            outer.push((product.pattern(&left), body));
        }
        let value = self.local_expr(if name == "showsPrec" { y } else { x }, o);
        let body = self.add(o, Kind::Case(value, outer));
        self.lambdas(o, vec![x, y], body)
    }
    pub(crate) fn derived_method(
        &mut self,
        instance: &TypedInstance,
        name: &str,
        o: Origin,
    ) -> ExprId {
        let CDecl::TypeDecl(_, _, variants, _) = &self.p.canonical.decls[instance.declaration].node
        else {
            unreachable!()
        };
        let products: Vec<_> = variants
            .iter()
            .map(|(name, fields)| {
                Product::Constructor(
                    GlobalName {
                        module: self.p.canonical.name.clone(),
                        name: name.clone(),
                    },
                    fields.len(),
                )
            })
            .collect();
        let dictionaries = instance
            .derived_fields
            .iter()
            .map(|fields| fields.iter().map(|d| self.dictionary(d, o)).collect())
            .collect::<Vec<_>>();
        self.generated_method(&products, &dictionaries, name, o)
    }
    /// One factory per interface/shape, shared across use sites and modules.
    /// Each call supplies the checker's ordered part dictionaries explicitly.
    pub(crate) fn structural(
        &mut self,
        interface: &str,
        shape: &StructuralShape,
        args: Vec<ExprId>,
        o: Origin,
    ) -> ExprId {
        let key = Global::Structural(interface.into(), shape.clone());
        if !self.out.globals.contains_key(&key) {
            let product = match shape {
                StructuralShape::Tuple(n) => Product::Tuple(*n),
                StructuralShape::Record(names) => Product::Record(names.clone()),
                StructuralShape::Unit => Product::Unit,
            };
            let params = self.fresh_fields(&product);
            let parts: Vec<_> = params.iter().map(|p| self.local_expr(*p, o)).collect();
            let self_id = self.fresh();
            let iface = knot_canonical::prelude::prelude()
                .interfaces
                .iter()
                .find(|i| i.name == interface)
                .unwrap();
            let mut fields = Vec::new();
            for (name, _) in &iface.methods {
                let body = if matches!(
                    name.as_str(),
                    "==" | "compare" | "showsPrec" | "fits" | "materialize"
                ) {
                    self.generated_method(
                        std::slice::from_ref(&product),
                        std::slice::from_ref(&parts),
                        name,
                        o,
                    )
                } else {
                    let f = self.add(
                        o,
                        Kind::Global(Global::Default(interface.into(), name.clone())),
                    );
                    let this = self.local_expr(self_id, o);
                    self.app(o, f, vec![this])
                };
                fields.push((Field::Method(interface.into(), name.clone()), body));
            }
            if interface == "Ord" {
                let eq_parts = parts
                    .iter()
                    .map(|p| self.add(o, Kind::Select(*p, Field::Super("Eq".into()))))
                    .collect();
                let eq = self.structural("Eq", shape, eq_parts, o);
                fields.push((Field::Super("Eq".into()), eq));
            }
            let dict = self.add(o, Kind::Dict(fields));
            let this = self.local_expr(self_id, o);
            let body = self.add(o, Kind::Let(vec![(self_id, dict)], this));
            let factory = self.lambdas(o, params, body);
            self.out.globals.insert(key.clone(), factory);
        }
        let factory = self.add(o, Kind::Global(key));
        self.app(o, factory, args)
    }
}
