//! A validated span predicts an element of a literal list on the next pick.
//! The Prelude list instance validates the whole list again on the candidate.
//! Its conjunction includes each omitted head and every suffix obligation.
use super::*;

struct Cursor {
    site: Arc<Site>,
    element: Ty,
    head: Ty,
    offset: usize,
    ty: Ty,
    request: TypedValue,
    values: TypedValue,
}

pub(super) struct Remainder {
    cursor: Cursor,
    fits: TypedValue,
}

impl Walker<'_> {
    pub(super) fn try_list_route(
        &mut self,
        site: &Site,
        request: &TypedValue,
        ty: &Ty,
    ) -> Result<bool, EditError> {
        if self.list_hints.is_empty()
            || site.synthetic
            || site.locked
            || site.fixed
            || self.has_candidate_values()
            || !self.adapters.is_empty()
            || site.module != self.snapshot.graph.entry().program.canonical.name
        {
            return Ok(false);
        }
        let (
            CExpr::List(elements),
            Some(Ty::Named(list, element)),
            Ty::Named(constraint, requests),
        ) = (&site.expr.node, &site.ty, ty)
        else {
            return Ok(false);
        };
        let ([element], [head]) = (element.as_slice(), requests.as_slice()) else {
            return Ok(false);
        };
        if list != "List"
            || constraint != "ListConstriction"
            || field_requests::payload(head).is_some()
        {
            return Ok(false);
        }
        let Some(index) = self.predicted_element(elements, 0) else {
            return Ok(false);
        };
        // A pinned argument contributes a separate validation obligation.
        if elements
            .iter()
            .any(|item| self.is_pinned(&site.child(item), 0))
        {
            return Ok(false);
        }
        let cursor = Cursor {
            site: Arc::new(site.clone()),
            offset: 0,
            ty: ty.clone(),
            head: head.clone(),
            element: element.clone(),
            values: self.source_value(site)?,
            request: request.clone(),
        };
        self.route_element(&cursor, index)
    }

    fn predicted_element(&self, elements: &[Spanned<CExpr>], start: usize) -> Option<usize> {
        // Hints are baseline byte ranges sorted in source order. A hint can
        // refer to a nested literal, an unrelated edit, or a different call.
        self.list_hints.iter().find_map(|span| {
            let offset = elements[start..].partition_point(|item| item.span.end <= span.start);
            let index = start + offset;
            let item = elements.get(index)?;
            (item.span.start <= span.start && span.end <= item.span.end).then_some(index)
        })
    }

    fn route_element(&mut self, cursor: &Cursor, index: usize) -> Result<bool, EditError> {
        let CExpr::List(elements) = &cursor.site.expr.node else {
            unreachable!()
        };
        let distance = index - cursor.offset;
        let payload = Ty::Tuple(vec![
            cursor.head.clone(),
            cursor.ty.clone(),
            Ty::from_value(cursor.values.value_type()),
            Ty::Named("Bool".into(), vec![]),
        ]);
        let operation = format!(
            r"\values wanted ->
  let seek n xs rs = case rs of
        Prepend r more -> case xs of
          x : rest -> if n == 0 then Just (r, more, rest, fits more rest)
                      else if fits r x then seek (n - 1) rest more else Nothing
          [] -> Nothing
        _ -> Nothing
  in seek {distance} values wanted"
        );
        let choice = self.probes.call_typed(
            &operation,
            &[cursor.values.clone(), cursor.request.clone()],
            &Ty::Named("Maybe".into(), vec![payload.clone()]),
        )?;
        let Some(parts) = self.probes.maybe_payload(&choice, &payload)? else {
            self.list_route_misses += 1;
            return Ok(false);
        };
        let head_request = self.probes.project_live(&parts, Field::Index(0))?;
        let tail_request = self.probes.project_live(&parts, Field::Index(1))?;
        let tail_values = self.probes.project_live(&parts, Field::Index(2))?;
        let tail_fits = self.probes.project_live(&parts, Field::Index(3))?;
        let mut target = cursor.site.child(&elements[index]);
        target.ty = Some(cursor.element.clone());
        self.schedule(target, head_request, cursor.head.clone())?;
        self.push_work(worklist::Work::ListRemainder(Box::new(Remainder {
            cursor: Cursor {
                site: cursor.site.clone(),
                element: cursor.element.clone(),
                head: cursor.head.clone(),
                offset: index + 1,
                ty: cursor.ty.clone(),
                request: tail_request,
                values: tail_values,
            },
            fits: tail_fits,
        })))?;
        self.list_route_hits += 1;
        Ok(true)
    }

    pub(super) fn resume_list_route(&mut self, rest: Remainder) -> Result<(), EditError> {
        // Demand the suffix only after the predicted element has finished.
        // A mismatch (including growth) uses the ordinary construction recipe.
        match self.probes.decode_live(&rest.fits)? {
            Data::Constructor(n, _) if n == knot_core::prelude("True") => return Ok(()),
            Data::Constructor(n, _) if n == knot_core::prelude("False") => {}
            _ => return Err(EditError::Internal("expected list suffix Bool".into())),
        }
        let cursor = rest.cursor;
        let CExpr::List(elements) = &cursor.site.expr.node else {
            unreachable!()
        };
        if let Some(next) = self.predicted_element(elements, cursor.offset) {
            if self.route_element(&cursor, next)? {
                return Ok(());
            }
        }
        let mut site = (*cursor.site).clone();
        for _ in 0..cursor.offset {
            self.visit_site()?;
            if self
                .probes
                .limits
                .runtime
                .cancellation
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(twine::Outcome::Cancelled.into());
            }
            let [_, tail] = site
                .list_fields()
                .ok_or_else(|| EditError::Internal("missing list suffix".into()))?;
            site = tail;
        }
        self.schedule(site, cursor.request, cursor.ty)
    }
}
