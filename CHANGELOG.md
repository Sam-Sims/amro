# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-10-31

Initial release.

### Added

- `single`, `paired` and `interleaved` modes
- Plain text and gzip compressed input
- Input from local files, stdin, HTTP and S3.
- SIMD accelerated sequence and quality validation, selected at runtime
- Human readable failure messages, `--json` output and `--quiet` for exit codes only.

