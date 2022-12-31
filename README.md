# gwipt

Automatic work-in-progress commits with descriptive commit messages generated
by GPT-3 Codex

Never again worry about the tension between "commit early, commit often" and
"every commit needs a commit message". All you need is an OpenAI API key, and
gwipt will track every single change in your working directory, on a parallel
`wip/` branch.

## Usage

Make sure the environment variable `OPENAI_API_KEY` is set to your personal
API key. Then, `cd` into the repository, and...

```bash
gwipt
```

Boom! Every change you make in the working tree is now saved, with a
descriptive commit message. Say you're on branch `A`; then all your changes
(including untracked files) will be automatically committed to `wip/A`, and you
can explore them whenever you want.

## Installation

```bash
cargo install gwipt
```
