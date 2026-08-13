use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use clap::{Parser, Subcommand};

// serde_yaml will look for a top level "jobs:" key
// automatically matching the field name jobs below
#[derive(Debug, Deserialize)]
struct Workflow {
    jobs: HashMap<String, Job>,
}

// what machine it runs on and the ordered list of steps
#[derive(Debug, Deserialize)]
struct Job {
    // yaml uses "runs-on" but hyphens arent allowed in
    // rust identifiers so rename explicitly
    #[serde(rename = "runs-on")]
    runs_on: String,
    steps: Vec<Step>,
}

// one step. name and env are optional since not every step has them
#[derive(Debug, Deserialize)]
struct Step {
    name: Option<String>,
    run: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Parser)]
#[command(name = "preflight-ci")]
#[command(about = "Run your CI workflow before you push")]
struct Cli {
    // tells clap the actual command lives inside commands enum
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // run a job from a workflow file
    Run {
        // no attribute = positional argument
        job: String,

        // default_value lets it fall back if omitted
        #[arg(long, default_value = "sample-workflow.yml")]
        workflow: String, 
    },
}

fn main() -> anyhow::Result<()> {
    // cli::parse() reads the arguements the user typed
    // and matches them against the shape above
    let cli = Cli::parse();

    let Commands::Run { job, workflow } = cli.command;

    let raw = fs::read_to_string(&workflow)?;
    let parsed: Workflow = serde_yaml::from_str(&raw)?;

    let job_def = parsed
        .jobs
        .get(&job)
        .ok_or_else(|| anyhow::anyhow!("job '{}' not found in workflow", job))?;

    println!("Job runs on: {}", job_def.runs_on);
    println!("Steps:");

    for step in &job_def.steps {
        let step_name = step.name.as_deref().unwrap_or("(unamed step)");
        let step_run = step.run.as_deref().unwrap_or("(no run command)");
        println!(" - {}: {}", step_name, step_run);
    }

    Ok(())
}