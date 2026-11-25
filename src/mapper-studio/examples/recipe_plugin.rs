//! Minimal language-neutral plugin example: one JSON request, one JSON response.
use mapper_studio::plugins::{API_VERSION, MAX_MESSAGE, Request, Response};
use std::io::{Read, Write};
fn main() {
    let result = (|| -> Result<Response, String> {
        let mut raw = Vec::new();
        std::io::stdin()
            .take((MAX_MESSAGE + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > MAX_MESSAGE {
            return Err("Request exceeds budget".into());
        }
        let request: Request = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        if request.protocol != API_VERSION {
            return Err("Unsupported protocol".into());
        }
        request.project.validate()?;
        let mut project = request.project;
        match request.action.as_str() {
            "shuffle-monuments" => {
                let steps = request
                    .parameters
                    .get("steps")
                    .map(|v| v.as_u64().ok_or("steps must be an integer"))
                    .transpose()?
                    .unwrap_or(1);
                if !(1..=1000).contains(&steps) {
                    return Err("steps must be 1..1000".into());
                }
                for _ in 0..steps {
                    project.shuffle_monuments();
                }
            }
            "set-monument-policy" => {
                project.performance.monuments = request
                    .parameters
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .ok_or("Provide an enabled boolean")?;
            }
            _ => return Err("Unknown action".into()),
        }
        project.validate()?;
        Ok(Response {
            protocol: API_VERSION,
            id: request.id,
            project: Some(project),
            error: None,
        })
    })();
    let response = result.unwrap_or_else(|error| Response {
        protocol: API_VERSION,
        id: 1,
        project: None,
        error: Some(error),
    });
    let mut output = std::io::stdout().lock();
    if serde_json::to_writer(&mut output, &response).is_err() || output.write_all(b"\n").is_err() {
        std::process::exit(1);
    }
}
