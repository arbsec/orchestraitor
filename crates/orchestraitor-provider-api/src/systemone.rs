//! `POST /v1/systemone` wire types for the Clef Flash decision model.
//!
//! The System One request carries a state plus typed questions; the response
//! answers every question with calibrated probabilities and **zero generated
//! text** (`output_tokens: 0`), which is what makes the decision model
//! dramatically cheaper than a chat-completion round trip (spec `30-model-routing.md` §9.45).
//!
//! Shapes verified live on `https://api.neuralwatt.com/v1/systemone`
//! (model id `clef-flash`, 2026-10-07):
//!
//! - request: `{"model": …, "state": …, "questions": {name: …}}`
//! - question type `choice`: `{"type": "choice", "criteria": {label: …}}`
//!   (criteria labels are the allowed answers) — answer
//!   `{"type": "choice", "choice": …, "confidence": …, "probabilities": {…}}`
//! - question type `noul`: `{"type": "noul", "instructions": …}` — answer
//!   `{"type": "noul", "noul": <probability>}`

use serde::Deserialize;

/// One System One question: a typed question the decision model answers in a
/// single pass. `name` is the answer key in the response's `answers` object.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemOneQuestion {
    /// Answer key this question is filed under.
    pub name: String,
    /// The typed question payload.
    pub question: SystemOneQuestionBody,
}

/// The typed payload of a [`SystemOneQuestion`].
#[derive(Debug, Clone, PartialEq)]
pub enum SystemOneQuestionBody {
    /// `choice`: pick one label from `criteria`; the labels are the allowed
    /// answers. `instructions` optionally refines how to choose.
    Choice {
        /// Allowed answers: label → inert description/criteria.
        criteria: Vec<(String, String)>,
        /// Optional free-text instructions.
        instructions: Option<String>,
    },
    /// `noul` ("no- ull" probability): a yes/no probability for the
    /// statement in `instructions`.
    Noul {
        /// The statement to score.
        instructions: String,
    },
}

impl SystemOneQuestionBody {
    /// The wire `type` literal.
    #[must_use]
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Choice { .. } => "choice",
            Self::Noul { .. } => "noul",
        }
    }
}

/// The `POST /v1/systemone` request body.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SystemOneRequest {
    /// Decision model id (for example `clef-flash`).
    pub model: String,
    /// The state the questions are evaluated against (text or JSON).
    pub state: String,
    /// Typed questions keyed by answer name.
    pub questions: serde_json::Map<String, serde_json::Value>,
}

/// The `POST /v1/systemone` response body (only the fields this adapter
/// consumes; unknown fields are ignored so the wire can evolve).
#[derive(Debug, Clone, Deserialize)]
pub struct SystemOneResponse {
    /// Per-question answers keyed by the request's question names.
    pub answers: serde_json::Map<String, serde_json::Value>,
    /// Token usage (a decision model reports `output_tokens: 0`).
    #[serde(default)]
    pub usage: Option<SystemOneUsage>,
}

/// Token usage reported by a System One response.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct SystemOneUsage {
    /// Prompt token count.
    #[serde(default)]
    pub input_tokens: u64,
    /// Generated token count — always `0` for a decision model.
    #[serde(default)]
    pub output_tokens: u64,
}

/// A parsed `choice` answer: the selected label and its calibrated
/// probability distribution over all labels.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemOneChoiceAnswer {
    /// The selected label.
    pub choice: String,
    /// Calibrated confidence for the selection.
    pub confidence: f64,
    /// Per-label probabilities when the provider reports them.
    pub probabilities: serde_json::Map<String, serde_json::Value>,
}

/// A parsed `noul` answer: the probability that the statement holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemOneNoulAnswer {
    /// Calibrated probability in `0.0..=1.0`.
    pub noul: f64,
}

/// Builds the wire JSON for one [`SystemOneQuestionBody`].
#[must_use]
pub fn question_body(body: &SystemOneQuestionBody) -> serde_json::Value {
    match body {
        SystemOneQuestionBody::Choice {
            criteria,
            instructions,
        } => {
            let mut criteria_map = serde_json::Map::new();
            for (label, description) in criteria {
                criteria_map.insert(
                    label.clone(),
                    serde_json::Value::String(description.clone()),
                );
            }
            let mut value = serde_json::Map::new();
            value.insert(
                "type".to_string(),
                serde_json::Value::String("choice".to_string()),
            );
            value.insert(
                "criteria".to_string(),
                serde_json::Value::Object(criteria_map),
            );
            if let Some(instructions) = instructions {
                value.insert(
                    "instructions".to_string(),
                    serde_json::Value::String(instructions.clone()),
                );
            }
            serde_json::Value::Object(value)
        }
        SystemOneQuestionBody::Noul { instructions } => serde_json::json!({
            "type": "noul",
            "instructions": instructions,
        }),
    }
}

/// Extracts a calibrated `choice` answer from the raw answer value.
///
/// # Errors
///
/// Returns a log-safe reason string when the answer is missing, has the
/// wrong shape, or carries a non-finite probability.
pub fn parse_choice_answer(
    question_name: &str,
    value: &serde_json::Value,
) -> Result<SystemOneChoiceAnswer, String> {
    let choice = value
        .get("choice")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("question '{question_name}': answer has no `choice` label"))?
        .to_string();
    let confidence = value
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("question '{question_name}': answer has no numeric confidence"))?;
    let probabilities = value
        .get("probabilities")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    Ok(SystemOneChoiceAnswer {
        choice,
        confidence,
        probabilities,
    })
}

/// Extracts a `noul` probability from the raw answer value.
///
/// # Errors
///
/// Returns a log-safe reason string when the answer is missing or has the
/// wrong shape.
pub fn parse_noul_answer(
    question_name: &str,
    value: &serde_json::Value,
) -> Result<SystemOneNoulAnswer, String> {
    let noul = value
        .get("noul")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| {
            format!("question '{question_name}': answer has no numeric `noul` probability")
        })?;
    Ok(SystemOneNoulAnswer { noul })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::float_cmp)]

    use super::*;

    #[test]
    fn choice_question_serializes_criteria_map() {
        let body = SystemOneQuestionBody::Choice {
            criteria: vec![("task-a".to_string(), "P0 fix".to_string())],
            instructions: Some("pick first".to_string()),
        };
        let json = question_body(&body);
        assert_eq!(json["type"], "choice");
        assert_eq!(json["criteria"]["task-a"], "P0 fix");
        assert_eq!(json["instructions"], "pick first");
    }

    #[test]
    fn noul_question_serializes_instructions() {
        let json = question_body(&SystemOneQuestionBody::Noul {
            instructions: "urgent?".to_string(),
        });
        assert_eq!(json["type"], "noul");
        assert_eq!(json["instructions"], "urgent?");
    }

    #[test]
    fn choice_answer_parses_from_live_shape() {
        // Shape captured live from POST /v1/systemone (clef-flash).
        let value: serde_json::Value = serde_json::from_str(
            r#"{"type":"choice","choice":"task-1","confidence":0.9875,
                "probabilities":{"task-1":0.9875,"task-2":0.007,"task-3":0.0055}}"#,
        )
        .unwrap();
        let answer = parse_choice_answer("pick", &value).unwrap();
        assert_eq!(answer.choice, "task-1");
        assert!((answer.confidence - 0.9875).abs() < 1e-9);
        assert_eq!(answer.probabilities.len(), 3);
    }

    #[test]
    fn noul_answer_parses_from_live_shape() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"type":"noul","noul":0.9632}"#).unwrap();
        let answer = parse_noul_answer("urgent", &value).unwrap();
        assert!((answer.noul - 0.9632).abs() < 1e-9);
    }

    #[test]
    fn malformed_answers_are_typed_rejections() {
        assert!(parse_choice_answer("pick", &serde_json::json!({})).is_err());
        assert!(parse_noul_answer("urgent", &serde_json::json!({"noul": "high"})).is_err());
    }
}
