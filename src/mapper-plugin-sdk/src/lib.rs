//! Standard input and output for Mapper API 1 recipe plugins.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub use serde_json::{Value, json};

pub const API_VERSION: u32 = 1;
pub const MAX_MESSAGE: usize = 4 * 1024 * 1024;
const MAX_ERROR: usize = 4096;

/// The recipe is a JSON object. Edit only the fields your action owns so new
/// or unrecognized recipe fields survive the round trip.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: u32,
    pub id: u64,
    pub action: String,
    pub parameters: Value,
    pub project: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub summary: String,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

pub enum Reply {
    Recipe(Value),
    Report(Report),
}

impl Reply {
    fn into_field(self) -> Result<(&'static str, Value), String> {
        match self {
            Self::Recipe(project) if project.is_object() => Ok(("project", project)),
            Self::Recipe(_) => Err("Plugin output must be a recipe object.".into()),
            Self::Report(report) => {
                report.validate()?;
                Ok((
                    "report",
                    serde_json::to_value(report).map_err(|error| error.to_string())?,
                ))
            }
        }
    }
}

impl Report {
    pub fn validate(&self) -> Result<(), String> {
        let valid_text = |text: &str| {
            !text.trim().is_empty() && text.len() <= 1024 && !text.chars().any(char::is_control)
        };
        if !valid_text(&self.summary)
            || self.findings.len() > 128
            || self
                .findings
                .iter()
                .any(|finding| !valid_text(&finding.message))
        {
            return Err("Plugin report exceeds its finding or text limits".into());
        }
        Ok(())
    }
}

/// Read a complete request, run the action, and write one response.
/// Action failures are returned to Mapper with the original request ID.
pub fn serve(
    input: impl Read,
    output: impl Write,
    handler: impl FnOnce(Request) -> Result<Value, String>,
) -> io::Result<()> {
    serve_response(input, output, Some("project"), |request| {
        handler(request).map(Reply::Recipe)
    })
}

/// Serve a read-only recipe inspection. Mapper validates the report again.
pub fn serve_inspection(
    input: impl Read,
    output: impl Write,
    handler: impl FnOnce(Request) -> Result<Report, String>,
) -> io::Result<()> {
    serve_response(input, output, Some("report"), |request| {
        handler(request).map(Reply::Report)
    })
}

fn serve_response(
    input: impl Read,
    mut output: impl Write,
    error_field: Option<&str>,
    handler: impl FnOnce(Request) -> Result<Reply, String>,
) -> io::Result<()> {
    let mut raw = Vec::new();
    input.take((MAX_MESSAGE + 2) as u64).read_to_end(&mut raw)?;
    if raw.last() == Some(&b'\n') {
        raw.pop();
    }
    if raw.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Plugin request exceeds 4 MiB",
        ));
    }
    let request: Request = serde_json::from_slice(&raw)?;
    if request.protocol != API_VERSION || !request.project.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid Mapper plugin request",
        ));
    }
    let id = request.id;
    let mut response = json!({"protocol":API_VERSION, "id":id, "error":null});
    if let Some(field) = error_field {
        response[field] = Value::Null;
    }
    let field = match handler(request).and_then(Reply::into_field) {
        Ok((field, value)) => {
            response[field] = value;
            Some(field)
        }
        Err(error) if !error.trim().is_empty() && error.len() <= MAX_ERROR => {
            response["error"] = Value::from(error);
            None
        }
        Err(_) => {
            response["error"] = json!("Plugin action failed without a usable error message.");
            None
        }
    };
    let mut encoded = serde_json::to_vec(&response)?;
    if encoded.len() >= MAX_MESSAGE {
        if let Some(field) = field {
            response[field] = Value::Null;
        }
        response["error"] = json!("Plugin response exceeds 4 MiB.");
        encoded = serde_json::to_vec(&response)?;
    }
    output.write_all(&encoded)?;
    output.write_all(b"\n")?;
    output.flush()
}

/// Serve a request received from the Mapper host process.
pub fn serve_stdio(handler: impl FnOnce(Request) -> Result<Value, String>) -> io::Result<()> {
    serve(io::stdin().lock(), io::stdout().lock(), handler)
}

/// Serve a read-only inspection received from Mapper.
pub fn serve_inspection_stdio(
    handler: impl FnOnce(Request) -> Result<Report, String>,
) -> io::Result<()> {
    serve_inspection(io::stdin().lock(), io::stdout().lock(), handler)
}

/// Serve an action in a package that has both recipe and inspection actions.
pub fn serve_action_stdio(
    handler: impl FnOnce(Request) -> Result<Reply, String>,
) -> io::Result<()> {
    serve_response(io::stdin().lock(), io::stdout().lock(), None, handler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_writes_findings_and_rejects_invalid_report_text() {
        let input = br#"{"protocol":1,"id":7,"action":"check","parameters":{},"project":{}}"#;
        let report = Report {
            summary: "Monument policy".into(),
            findings: vec![Finding {
                severity: Severity::Warning,
                message: "Review monument targets.".into(),
            }],
        };
        let mut output = Vec::new();
        serve_inspection(input.as_slice(), &mut output, |_| Ok(report.clone())).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            json!({"protocol":1,"id":7,"report":report,"error":null})
        );
        output.clear();
        serve_inspection(input.as_slice(), &mut output, |_| {
            Ok(Report {
                summary: "Invalid\ntext".into(),
                findings: Vec::new(),
            })
        })
        .unwrap();
        let rejected: Value = serde_json::from_slice(&output).unwrap();
        assert!(rejected["report"].is_null());
        assert!(rejected["error"].is_string());
    }

    #[test]
    fn retains_request_id_and_unrecognized_recipe_fields() {
        let input = br#"{"protocol":1,"id":42,"action":"mark","parameters":{},"project":{"future":{"settings":[1,2]}}}"#;
        let mut output = Vec::new();
        serve(input.as_slice(), &mut output, |mut request| {
            assert_eq!(request.action, "mark");
            request.project["marked"] = json!(true);
            Ok(request.project)
        })
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            json!({"protocol":1,"id":42,"project":{"future":{"settings":[1,2]},"marked":true},"error":null})
        );
    }

    #[test]
    fn rejects_bad_protocol_and_reports_oversized_output_to_the_host() {
        let request = |protocol| {
            serde_json::to_vec(&json!({
                "protocol": protocol, "id": 9, "action": "sample",
                "parameters": {}, "project": {},
            }))
            .unwrap()
        };
        let mut output = Vec::new();
        assert!(serve(request(2).as_slice(), &mut output, |_| unreachable!()).is_err());
        assert!(output.is_empty());
        serve(request(1).as_slice(), &mut output, |_| {
            Ok(json!({"large": "a".repeat(MAX_MESSAGE)}))
        })
        .unwrap();
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["id"], 9);
        assert!(response["project"].is_null());
        assert_eq!(response["error"], "Plugin response exceeds 4 MiB.");
    }
}
