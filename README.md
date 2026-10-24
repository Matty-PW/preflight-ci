# preflight-ci

[![CI](https://github.com/Matty-PW/preflight-ci/actions/workflows/ci.yml/badge.svg)](https://github.com/Matty-PW/preflight-ci/actions/workflows/ci.yml)

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
- Generates a unique container name per run so a crash previous run does not block a new one
- Finds your workflow automatically in `.github/workflows/`, or takes an explicit path with `--workflow`
- Mounts your project folder into the container at `/workspace`, so steps run against your real source code
- Always removes the container afterwards, even if a step hits an error

## Limitations / Notes

- `uses:` steps (3rd party github actions) are not supported, they are currently skipped and counted as passed
- Only ubuntu runners are supported. macOS and Windows jobs fail with error messages rather than running incorrectly
- Matrix combinations are run sequentially, not in parallel
- No caching between runs beyond Docker's own image cache, each run recreates its container from scratch
- no workflow-level or job-level env:, `if:` conditionals, `needs:` job dependencies or reusable workflows
- Your project folder is mounted live, not copied, so steps that write files (such as `cargo build`) write to your real folder. Linux build output in `target/` can confuse a macOS build, so run `cargo clean` if that happens
- Containers start from a bare Ubuntu image with no toolchains, so a workflow that needs Rust, Node or Python must install it in a `run:` step
- Pressing Ctrl+C mid-run can leave a `preflight-ci-*` container behind (check with `docker ps` and remove it with `docker rm -f <name>`)
  

## Installation

    cargo build --release
    cargo install --path .
This installs `preflight-ci` to `~/.cargo/bin`, which is on your path if you installed Rust via rustup

## Usage
From the root of your project:

    preflight-ci run <job-name>

preflight-ci searches `.github/workflows/` for a file that defines the job. If no file defines it, or more than one does, it stops with an error. In the second case, pick a file explicitly:

    preflight-ci run <job-name> --workflow path/to/workflow.yml

Example:

    preflight-ci run test --workflow examples/sample-workflow.yml

  Matrix example - a job like this runs once per value in the matrix

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
