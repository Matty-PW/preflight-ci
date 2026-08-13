# preflight-ci

A CLI tool that runs your GitHub Actions workflow locally, so you know
whether your CI pipeline will pass before you push.

## Why this exists

GitHub Actions only tells you whether your workflow passed after you have
already pushed and it's run on GitHub's servers. This tool runs the same
workflow file locally first, so you get instant feedback while iterating
on a CI config, without having to push and wait every time.

## Status: work in progress

Currently working:
- Parses GitHub Actions workflow YAML files
- CLI interface: `preflight-ci run <job-name> --workflow <path>`
- Lists the steps a given job would execute, with clear errors for
  missing files or unknown job names

Not yet built:
- Actually executing steps inside a Docker container
- Streaming live output as steps run
- Environment variable / secrets injection
- Matrix build support

## Usage

    cargo run -- run <job-name> --workflow path/to/workflow.yml

Example:

    cargo run -- run test --workflow sample-workflow.yml

## Tech stack

- Rust
- clap — CLI argument parsing
- serde / serde_yaml — parsing the workflow YAML
- tokio + bollard — Docker container execution
