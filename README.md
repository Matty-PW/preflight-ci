# preflight-ci

A CLI tool that runs your GitHub Actions workflow locally, in an isolated Docker container, so you know
whether your CI pipeline will pass before you push.

## Why

GitHub Actions only tells you whether your workflow passed after you have
already pushed and it's run on GitHub's servers. This tool runs the same
workflow file locally first, so you get instant feedback while iterating
on a CI config, without having to push and wait every time.

## What it does

- Parses GitHub Actions workflow YAML files
- Runs each step inside an isolated, disposable Docker container, with live streaming output
- Checks exit codes, so pass/fail reflects what happened
- Injects step level environment variables from the workflow file
- Automatically pulls missing images with a live progress bar
- Rejects `macos-*` and `windows-*` runners with error message
- Supports `strategy.matrix` builds
- Generates a unique container name per run so a crash previous run does block a new one
- CLI interface: `preflight-ci run <job-name> --workflow <path>`

## Limitations

- `uses:` steps (3rd party github actions) are not supported, they are currently skipped and counted as passed
- Only ubuntu runners are supported. macOS and Windows jobs fail with error messages rather than running incorrently
- Matrix combinations are run sequentially, not in parallel
- No caching between runs beyond Dockers own image cache, each run recreates its container from scratch
- Only a subset of real workflow is understood - no `if:` conditionals, `needs:` job dependencies or reusable workflows

## Installation

    cargo build --release
    cargo install --path .
This installs `preflight-ci` to `~/.cargo/bin`, which is on your path if you installed Rust via rustup

## Usage

    preflight-ci run <job-name> --workflow path/to/workflow.yml

Examples:

    preflight-ci run test --workflow sample-workflow.yml

  Matrix exampple - a job like this runs once per value in the matrix

    jobs:
      test:
        runs-on: ubuntu-latest
        strategy:
          matrix:
            greeting: [hello, helo, hi]
        steps:
          - name: Say hello
            run: echo "${{ matrix.greeting }} from preflight-ci"

  Testing

    cargo test

## Tech stack

- Rust
- clap - CLI argument parsing
- serde / serde_yaml - parsing the workflow YAML
- tokio + bollard - Docker container execution
- indicatif - terminal progress bar for image pulls
