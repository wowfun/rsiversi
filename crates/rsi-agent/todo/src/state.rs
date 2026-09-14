use serde::{Deserialize, Serialize};

/// One model-declared task state; completion is not independent verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Work has not started.
    Pending,
    /// Work is active, possibly alongside other active items.
    InProgress,
    /// The model reports this work as finished.
    Completed,
}

/// One bounded task line.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    content: String,
    status: TodoStatus,
}
impl TodoItem {
    /// Creates a nonempty single-line task with at most 512 UTF-8 bytes.
    ///
    /// # Errors
    /// Rejects empty content, control characters and oversized text.
    pub fn new(content: String, status: TodoStatus) -> Result<Self, String> {
        let item = Self { content, status };
        item.validate()?;
        Ok(item)
    }
    /// Returns the human-readable task.
    pub fn content(&self) -> &str {
        &self.content
    }
    /// Returns its declared status.
    pub const fn status(&self) -> TodoStatus {
        self.status
    }
    fn validate(&self) -> Result<(), String> {
        if self.content.trim().is_empty()
            || self.content.len() > 512
            || self.content.chars().any(char::is_control)
        {
            return Err("Todo content must be one nonempty line of at most 512 UTF-8 bytes".into());
        }
        Ok(())
    }
}
impl<'de> Deserialize<'de> for TodoItem {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            content: String,
            status: TodoStatus,
        }
        let wire = Wire::deserialize(d)?;
        Self::new(wire.content, wire.status).map_err(serde::de::Error::custom)
    }
}

/// A complete authoritative list, empty when cleared or freshly forked.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TodoList(Vec<TodoItem>);
impl TodoList {
    /// Validates one complete replacement (64 items and 64 KiB encoded maximum).
    ///
    /// # Errors
    /// Rejects invalid items or a list exceeding either bound.
    pub fn new(items: Vec<TodoItem>) -> Result<Self, String> {
        let list = Self(items);
        list.validate()?;
        Ok(list)
    }
    /// Returns task order as supplied by the model.
    pub fn items(&self) -> &[TodoItem] {
        &self.0
    }
    /// Checks list bounds using the immutable items' construction proofs.
    ///
    /// # Errors
    /// Rejects an exceeded list bound.
    pub fn validate(&self) -> Result<(), String> {
        if self.0.len() > 64 {
            return Err("Todo list exceeds 64 items".into());
        }
        let encoded = 2
            + self.0.len().saturating_sub(1)
            + self
                .0
                .iter()
                .map(|item| {
                    // Controls are excluded by TodoItem; only quotes and backslashes escape.
                    let status = match item.status {
                        TodoStatus::Pending => "pending",
                        TodoStatus::InProgress => "in_progress",
                        TodoStatus::Completed => "completed",
                    };
                    r#"{"content":"","status":""}"#.len()
                        + status.len()
                        + item.content.len()
                        + item
                            .content
                            .bytes()
                            .filter(|byte| matches!(byte, b'"' | b'\\'))
                            .count()
                })
                .sum::<usize>();
        if encoded > 64 * 1024 {
            return Err("Todo state exceeds 64 KiB".into());
        }
        Ok(())
    }
}
impl<'de> Deserialize<'de> for TodoList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(Vec::<TodoItem>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_decode_preserves_parallel_work_and_clear() {
        let list: TodoList = serde_json::from_str(r#"[{"content":"first","status":"in_progress"},{"content":"second","status":"in_progress"}]"#).unwrap();
        assert_eq!(list.items().len(), 2);
        assert_eq!(
            serde_json::from_str::<TodoList>("[]").unwrap(),
            TodoList::default()
        );
        for value in [
            r#"[{"content":"","status":"pending"}]"#,
            r#"[{"content":"line\nline","status":"pending"}]"#,
            r#"[{"content":"valid","status":"unknown"}]"#,
            r#"[{"content":"valid","status":"pending","extra":true}]"#,
        ] {
            assert!(serde_json::from_str::<TodoList>(value).is_err());
        }
    }
    #[test]
    fn encoded_bound_agrees_with_serde_for_escaped_and_multibyte_items() {
        for status in [
            TodoStatus::Pending,
            TodoStatus::InProgress,
            TodoStatus::Completed,
        ] {
            for content in [
                "x".repeat(512),
                "字".repeat(170),
                "\\\"".repeat(256),
                format!("x{}", "\u{2028}".repeat(170)),
            ] {
                let item = TodoItem::new(content, status).unwrap();
                for count in 0..=65 {
                    let items = vec![item.clone(); count];
                    let expected =
                        count <= 64 && serde_json::to_vec(&items).unwrap().len() <= 64 * 1024;
                    assert_eq!(TodoList::new(items).is_ok(), expected);
                }
            }
        }
    }
    #[test]
    fn item_utf8_list_count_and_encoded_bytes_are_independent_bounds() {
        assert!(TodoItem::new("字".repeat(171), TodoStatus::Pending).is_err());
        let item = TodoItem::new("x".repeat(512), TodoStatus::Pending).unwrap();
        assert!(TodoList::new(vec![item.clone(); 64]).is_ok());
        assert!(TodoList::new(vec![item; 65]).is_err());
        let escaping = TodoItem::new("\\".repeat(512), TodoStatus::Pending).unwrap();
        assert!(TodoList::new(vec![escaping; 64]).is_err());
    }
}
