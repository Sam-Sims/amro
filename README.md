# AMRO - Are my reads ok?

SIMD accelerated FASTQ validation becuase why the hell not. Validates FASTQ files at basically disk I/O speed (I max out at ~2.11 GB/s on my cheap PCIe3.0 NVMe rated for 2.1 GB/s). With files cached in memory, amro validates nanopore reads at ~24 GB/s and Illumina paired reads at ~15 GB/s.

## Why?

amro started as a small personal tool to support my use case: quickly sanity check lots of FASTQ files in *quick enough* time*, support remote filesystems such as S3 and HTTP, and output a helpful, human readable failure message with optional JSON output for parsing in pipelines. The v1.0 release of the cool [fearless_simd](https://github.com/linebender/fearless_simd) crate inspired me to have a play around with quality and sequence string validation as they were by far the slowest aspect. And voila the slightly ridiculous concept of SIMD FASTQ validation was born.

In practice *it is* actually a useful tool to run on FASTQ files before sending them to anyone else (including uploading to the SRA/ENA etc), or recieving them from some otherwise untrusted input - ensuring that at the very least, the FASTQ files look valid and wont break something odd downstream. The idea is that it is fast+unobstructive enough that there is no reason not to run it.

**In fairness it was quick enough - the SIMD and other optimisations were mainly done for fun.*

## What?

In essence, amro validates that a FASTQ file appears valid. The closest thing to a FASTQ spec is [Cock et al](https://doi.org/10.1093/nar/gkp1137). amro ensures your FASTQ file meets as many rules as possible including a few extra (such as validating paired files), in the shortest time possible.

## Install

### A note on SIMD:

amro uses [fearless_simd](https://github.com/linebender/fearless_simd), which means the binary is portable and SIMD will work on any supported machine, i.e not dependent on building from source. It selects the most appropriate instruction set at runtime (falling back to scalar if no SIMD support).

### Conda:

### Binaries:

Precompiled binaries for Linux, MacOS are attached to the latest [release](https://github.com/Sam-Sims/amro/releases).

### Cargo:

Requires [cargo](https://www.rust-lang.org/tools/install)

```bash
cargo install amro
```

### Build from source:

#### Install rust toolchain:

To install please refer to the rust documentation: [docs](https://www.rust-lang.org/tools/install).

#### Clone the repository:

```bash
git clone https://github.com/Sam-Sims/amro
```

#### Build and add to path:

```bash
cd amro
cargo build --release
export PATH=$PATH:$(pwd)/target/release
```

All executables will be in the directory `amro/target/release`.

## Usage

### How to run

```bash
amro single <reads.fastq>
amro paired <reads_R1.fastq> <reads_R2.fastq>
amro interleaved <reads.fastq>
```

A common use is running over a folder of fastqs (will only show failing FASTQs):

```bash
# single files
for f in fastq_files/*.fastq.gz; do amro single --quiet "$f" || echo "FAILED: $f"; done
## or paired matching _R1 and _R2
for r1 in *_R1*; do amro paired --quiet "$r1" "${r1/_R1/_R2}" || echo "FAILED: $r1"; done
```

FASTQ input can be plain text or gzip compressed.

amro will stop at the first validation error it encounters and not continue processing. In general it is designed to be fairly unobstructive and will exit 0 with a short message if the reads pass, or exit 1 with a message describing the error. If you want exit codes only you can use `--quiet` and if you want json output, use `--json`. An exit code of 2 means something went wrong before the fastq file could start validation.

### Remote inputs

Any input can be an `http://` URL or `s3://` URI instead of a local path.

```bash
amro single https://example.com/reads.fastq.gz
amro paired s3://my-bucket/run1/reads_R1.fastq.gz s3://my-bucket/run1/reads_R2.fastq.gz
```

Local plaintext files are read in parallel, but remote inputs are read as a single stream, so they'll usually be limited by network speed.

### Benchmarks
You can also run the benchmarks:

```bash
cargo bench
```

### Options

```
Are your FASTQ files ok?

Usage: amro <COMMAND>

Commands:
  single       Validate a single end FASTQ file
  paired       Validate a set of paired end FASTQ files
  interleaved  Validate an interleaved FASTQ file of alternating R1 and R2 mates
  rules        List AMRO rules
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

`single`, `paired` and `interleaved` all take the same options:

```
Validate a single end FASTQ file

Usage: amro single [OPTIONS] <INPUT>

Arguments:
  <INPUT>


Options:
      --alphabet <ALPHABET>
          Bases a sequence may contain.

          A sequence with any other byte fails invalid-sequence-character.

          Possible values:
          - standard: Uppercase A, C, G, T and N only
          - iupac:    Uppercase IUPAC nucleotide codes (A C G T U R Y S W K M B D H V N)

          [default: standard]

      --quality-encoding <QUALITY_ENCODING>
          Bytes a quality string may contain, depending on how qualities were encoded.

          A quality with any other byte fails invalid-quality-byte.

          Possible values:
          - phred33: Bytes ! to ~ (33 to 126)
          - phred64: Bytes @ to ~ (64 to 126)

          [default: phred33]

      --min-length <BASES>
          Shortest sequence length allowed.

          A sequence shorter than this fails the short-read rule. The default rejects empty reads, use 0 to allow them.

          [default: 1]

      --allow <CODE>
          Disable default rules.

          Codes can be comma-separated or given in repeated flags. To allow empty or short reads, see --min-length.

          Possible values:
          - header-too-long:        Allow headers longer than 255 bytes
          - lowercase-sequence:     Allow lowercase bases in sequences
          - sequence-all-same-byte: Allow sequences made of one repeated base
          - quality-all-same-byte:  Allow quality strings made of one repeated byte

      --quiet
          Disable all output, the outcome is reported by exit code only

      --json
          Print the result as JSON

  -h, --help
          Print help (see a summary with '-h')
```

## Example

```bash
amro single my.fastq

No, your reads are not ok.

invalid-sequence-character: 'R' is not in the standard alphabet.

   file:     my.fastq
   record:   2
   line:     6
   column:   3

Hint:
   if the sequences use IUPAC codes, pass --alphabet iupac

To view this record:
   sed -n '5,8p;8q' my.fastq
```

If nothing is wrong:

```bash
amro single my.fastq

Yes, your reads are ok.
```

## Rules

Some rules are optional, but all rules are enabled by default to be as strict as possible.

For paired and interleaved reads, amro also checks the mates match and will flag singletons.

```json
amro rules --json
{
  "rules": [
    {
      "code": "input-read-failed",
      "scope": "input",
      "message": "FASTQ could not be read to the end of the file.",
      "opt-out": null
    },
    {
      "code": "empty-input",
      "scope": "input",
      "message": "FASTQ contains no records.",
      "opt-out": null
    },
    {
      "code": "invalid-header-marker",
      "scope": "record",
      "message": "Header line does not start with '@'.",
      "opt-out": null
    },
    {
      "code": "truncated-final-record",
      "scope": "record",
      "message": "FASTQ ends before a complete record can be formed.",
      "opt-out": null
    },
    {
      "code": "invalid-header-character",
      "scope": "record",
      "message": "Header contains a byte that is not printable ASCII.",
      "opt-out": null
    },
    {
      "code": "empty-read-identifier",
      "scope": "record",
      "message": "Header has no read identifier after '@'.",
      "opt-out": null
    },
    {
      "code": "invalid-separator-marker",
      "scope": "record",
      "message": "Separator line does not start with '+'.",
      "opt-out": null
    },
    {
      "code": "separator-header-mismatch",
      "scope": "record",
      "message": "Separator contains a header that differs from the main header.",
      "opt-out": null
    },
    {
      "code": "invalid-sequence-character",
      "scope": "record",
      "message": "Sequence contains a character outside the allowed alphabet.",
      "opt-out": null
    },
    {
      "code": "invalid-quality-byte",
      "scope": "record",
      "message": "Quality contains a character outside the encoding range.",
      "opt-out": null
    },
    {
      "code": "sequence-quality-length-mismatch",
      "scope": "record",
      "message": "Sequence and quality lengths differ.",
      "opt-out": null
    },
    {
      "code": "header-too-long",
      "scope": "record",
      "message": "Header is longer than 255 bytes.",
      "opt-out": "--allow header-too-long"
    },
    {
      "code": "lowercase-sequence",
      "scope": "record",
      "message": "Sequence contains lowercase bases.",
      "opt-out": "--allow lowercase-sequence"
    },
    {
      "code": "short-read",
      "scope": "record",
      "message": "Sequence is shorter than the minimum read length.",
      "opt-out": "--min-length 0"
    },
    {
      "code": "sequence-all-same-byte",
      "scope": "record",
      "message": "Sequence consists of one repeated base.",
      "opt-out": "--allow sequence-all-same-byte"
    },
    {
      "code": "quality-all-same-byte",
      "scope": "record",
      "message": "Quality string consists of one repeated character.",
      "opt-out": "--allow quality-all-same-byte"
    },
    {
      "code": "mate-mismatch",
      "scope": "pair",
      "message": "Paired reads do not have the same read identifier.",
      "opt-out": null
    },
    {
      "code": "singleton",
      "scope": "pair",
      "message": "Read has no mate in the other input.",
      "opt-out": null
    }
  ]
}
```
