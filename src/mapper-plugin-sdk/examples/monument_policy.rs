use mapper_plugin_sdk::{
    Finding, Reply, Report, Request, Severity, Value, json, serve_action_stdio,
};

fn set_policy(mut request: Request) -> Result<Value, String> {
    let enabled = request.parameters["enabled"]
        .as_bool()
        .ok_or("Provide an enabled boolean.")?;
    let recipe = request
        .project
        .as_object_mut()
        .ok_or("Expected a recipe.")?;
    let performance = recipe
        .entry("performance")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Expected performance settings.")?;
    performance.insert("monuments".into(), json!(enabled));
    Ok(request.project)
}

fn check_policy(request: Request) -> Result<Report, String> {
    let targets = request.project["targets"]
        .as_array()
        .ok_or("Expected monument targets.")?;
    let required = targets
        .iter()
        .filter(|target| target["presence"] == "Required")
        .count();
    let enabled = request.project["performance"]["monuments"]
        .as_bool()
        .unwrap_or(true);
    Ok(Report {
        summary: format!("{required} required monument targets"),
        findings: if !enabled && required > 0 {
            vec![Finding {
                severity: Severity::Warning,
                message: "This recipe disables monument generation.".into(),
            }]
        } else {
            Vec::new()
        },
    })
}

fn main() -> std::io::Result<()> {
    serve_action_stdio(|request| match request.action.as_str() {
        "set-monument-policy" => set_policy(request).map(Reply::Recipe),
        "check-monument-policy" => check_policy(request).map(Reply::Report),
        _ => Err("Unknown action.".into()),
    })
}
