//! Checked byte-range relocation while composing and indenting source fragments.
use super::*;
pub(super) struct Text {
    pub text: String,
    pub locations: Vec<((u32, u32), Span)>,
    limit: usize,
}
impl Text {
    pub fn new(text: &str, limit: usize) -> Result<Self, EditError> {
        if text.len() > limit {
            return Err(EditError::SourceTooLarge);
        }
        Ok(Self {
            text: text.into(),
            locations: vec![],
            limit,
        })
    }
    pub fn push(&mut self, text: &str) -> Result<(), EditError> {
        if self.text.len().saturating_add(text.len()) > self.limit {
            return Err(EditError::SourceTooLarge);
        }
        self.text.push_str(text);
        Ok(())
    }
    pub fn append(&mut self, other: Self) -> Result<(), EditError> {
        let offset = u32::try_from(self.text.len()).map_err(|_| EditError::SourceTooLarge)?;
        self.push(&other.text)?;
        for (key, span) in other.locations {
            let start = offset
                .checked_add(span.start)
                .ok_or(EditError::SourceTooLarge)?;
            let end = offset
                .checked_add(span.end)
                .ok_or(EditError::SourceTooLarge)?;
            self.locations.push((key, Span::new(start, end)));
        }
        Ok(())
    }
    pub fn indented(mut self, width: usize, first: bool) -> Result<Self, EditError> {
        let newlines = self
            .text
            .bytes()
            .enumerate()
            .filter_map(|(i, c)| (c == b'\n').then_some(i))
            .collect::<Vec<_>>();
        let extra = (newlines.len() + usize::from(first))
            .checked_mul(width)
            .ok_or(EditError::SourceTooLarge)?;
        if self.text.len().saturating_add(extra) > self.limit {
            return Err(EditError::SourceTooLarge);
        }
        let shift = |position: u32| -> Result<u32, EditError> {
            let count = newlines.partition_point(|&newline| newline < position as usize)
                + usize::from(first);
            u32::try_from(position as usize + count * width).map_err(|_| EditError::SourceTooLarge)
        };
        for (_, span) in &mut self.locations {
            *span = Span::new(shift(span.start)?, shift(span.end)?);
        }
        let padding = " ".repeat(width);
        self.text = format!(
            "{}{}",
            if first { &padding } else { "" },
            self.text.replace('\n', &format!("\n{padding}"))
        );
        Ok(self)
    }
}
