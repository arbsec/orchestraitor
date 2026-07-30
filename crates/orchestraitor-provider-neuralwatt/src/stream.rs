//! SSE stream parsing for Neuralwatt Chat Completions streaming responses.
//!
//! The transport consumes [`reqwest::Response::bytes_stream`] and feeds
//! chunks into an [`SseParser`], which splits the byte stream into
//! individual SSE events and converts them into [`ModelEvent`] values.

use orchestraitor_provider_api::{ModelEvent, TokenCount};
use serde_json::Value;

use crate::error::NeuralwattError;
use crate::wire::StreamChunk;
/// SSE sentinel marking the end of a stream.
const SSE_DONE: &str = "[DONE]";

/// Parses a byte stream of SSE data into [`ModelEvent`] values.
///
/// The parser buffers incoming bytes, splits on `\n\n` (the SSE event
/// delimiter), and extracts `data:` lines. Each data line is either
/// `[DONE]` (emitting [`ModelEvent::Completed`]) or a JSON chunk
/// (parsed into [`StreamChunk`] and converted to events).
pub struct SseParser {
    buffer: String,
    done: bool,
}

impl SseParser {
    /// Creates a new SSE parser with an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            done: false,
        }
    }

    /// Feeds raw bytes into the parser and returns any complete events.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattError::InvalidStreamEvent`] when a data line
    /// cannot be parsed as JSON.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ModelEvent>, NeuralwattError> {
        if self.done {
            return Ok(Vec::new());
        }
        let text = String::from_utf8_lossy(bytes);
        self.buffer.push_str(&text);
        self.drain_complete_events()
    }

    /// Flushes any remaining buffered data as a final event.
    ///
    /// # Errors
    ///
    /// Returns [`NeuralwattError::InvalidStreamEvent`] when buffered data
    /// cannot be parsed.
    pub fn finish(&mut self) -> Result<Vec<ModelEvent>, NeuralwattError> {
        if self.done {
            return Ok(Vec::new());
        }
        let trimmed = self.buffer.trim().to_string();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let events = self.parse_event(&trimmed)?;
        self.done = true;
        Ok(events)
    }

    /// Returns whether the parser has seen `[DONE]`.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn drain_complete_events(&mut self) -> Result<Vec<ModelEvent>, NeuralwattError> {
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.find("\n\n") {
            let event_text = self.buffer[..pos].to_string();
            self.buffer = self.buffer[pos + 2..].to_string();
            let trimmed = event_text.trim();
            if trimmed.is_empty() {
                continue;
            }
            events.extend(self.parse_event(trimmed)?);
        }
        Ok(events)
    }

    fn parse_event(&mut self, text: &str) -> Result<Vec<ModelEvent>, NeuralwattError> {
        let data = extract_data_line(text);
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data.trim() == SSE_DONE {
            self.done = true;
            return Ok(vec![ModelEvent::Completed]);
        }
        let chunk: StreamChunk = serde_json::from_str(&data)
            .map_err(|source| NeuralwattError::ResponseParse { source })?;
        Ok(chunk_to_events(&chunk))
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Extracts the `data:` payload from an SSE event block.
fn extract_data_line(text: &str) -> String {
    text.lines()
        .filter_map(|line| {
            let trimmed = line
                .strip_prefix("data:")
                .or_else(|| line.strip_prefix("data: "))?;
            Some(trimmed.trim_start().to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Converts a [`StreamChunk`] into zero or more [`ModelEvent`] values.
fn chunk_to_events(chunk: &StreamChunk) -> Vec<ModelEvent> {
    let mut events = Vec::new();
    for choice in &chunk.choices {
        if let Some(content) = &choice.delta.content
            && !content.is_empty()
        {
            events.push(ModelEvent::TextDelta {
                text: content.clone(),
            });
        }
        for tool_call in &choice.delta.tool_calls {
            if let Some(function) = &tool_call.function {
                let id = tool_call.id.clone().unwrap_or_default();
                let name = function.name.clone().unwrap_or_default();
                let arguments = function
                    .arguments
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(std::string::ToString::to_string)
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .unwrap_or(Value::Null);
                if !name.is_empty() {
                    events.push(ModelEvent::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }
    }
    if let Some(usage) = &chunk.usage {
        events.push(ModelEvent::Usage {
            token_count: TokenCount {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cached_tokens: 0,
                reasoning_tokens: 0,
            },
        });
    }
    events
}

/// Converts a non-streaming [`crate::wire::ChatCompletionResponse`] into
/// [`ModelEvent`] values.
pub(crate) fn response_to_events(
    response: &crate::wire::ChatCompletionResponse,
) -> Vec<ModelEvent> {
    let mut events = vec![ModelEvent::Started];
    for choice in &response.choices {
        if let Some(content) = &choice.message.content
            && !content.is_empty()
        {
            events.push(ModelEvent::TextDelta {
                text: content.clone(),
            });
        }
        for tool_call in &choice.message.tool_calls {
            let arguments =
                serde_json::from_str::<Value>(&tool_call.function.arguments).unwrap_or(Value::Null);
            events.push(ModelEvent::ToolCall {
                id: tool_call.id.clone(),
                name: tool_call.function.name.clone(),
                arguments,
            });
        }
    }
    if let Some(usage) = &response.usage {
        events.push(ModelEvent::Usage {
            token_count: TokenCount {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cached_tokens: 0,
                reasoning_tokens: 0,
            },
        });
    }
    events.push(ModelEvent::Completed);
    events
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn parser_handles_single_chunk() {
        let mut parser = SseParser::new();
        let events = parser
            .feed(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ModelEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    #[test]
    fn parser_handles_done_sentinel() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: [DONE]\n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ModelEvent::Completed));
        assert!(parser.is_done());
    }

    #[test]
    fn parser_buffers_partial_events() {
        let mut parser = SseParser::new();
        let events = parser
            .feed(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}")
            .unwrap();
        assert!(events.is_empty());
        let events = parser.feed(b",\"finish_reason\":null}]}\n\n").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parser_handles_usage_on_final_chunk() {
        let mut parser = SseParser::new();
        let events = parser
            .feed(b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ModelEvent::Usage { token_count } => {
                assert_eq!(token_count.input_tokens, 10);
                assert_eq!(token_count.output_tokens, 5);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parser_handles_tool_call_streaming() {
        let mut parser = SseParser::new();
        let events = parser
            .feed(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"NYC\\\"}\"}}]}}]}\n\n")
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ModelEvent::ToolCall { id, name, .. } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "get_weather");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parser_handles_multiple_events_in_one_feed() {
        let mut parser = SseParser::new();
        let input = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n";
        let events = parser.feed(input).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parser_finishes_remaining_buffer() {
        let mut parser = SseParser::new();
        parser
            .feed(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}")
            .unwrap();
        let events = parser.finish().unwrap();
        assert_eq!(events.len(), 1);
    }
}
