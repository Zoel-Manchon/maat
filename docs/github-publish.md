# Publish Maat on GitHub

## Repository identity

**Name**

```text
maat
```

**Description**

```text
A retro modal terminal editor in Rust with Vim-inspired motions and SHA-256 integrity checks.
```

**Topics**

```text
rust ratatui tui terminal text-editor vim modal-editor sha256 cybersecurity cli
```

## Fast path

Install and authenticate GitHub CLI, open Git Bash, WSL, Linux or macOS in the project directory, and run:

```bash
./scripts/publish-github.sh
```

The script initializes Git, creates the first commit, creates the public
`Zoel-Manchon/maat` repository, pushes `main` and adds the recommended topics.

To use another owner, repository name or visibility:

```bash
./scripts/publish-github.sh Zoel-Manchon maat private
```

Configure your Git identity first when needed:

```bash
git config --global user.name "Zoel Arias Manchón"
git config --global user.email "YOUR_GITHUB_EMAIL"
```

## Manual path with Git Bash

```bash
git init
git add .
git commit -m "feat: publish Maat 0.3.0"
git branch -M main

gh auth login
gh repo create Zoel-Manchon/maat \
  --public \
  --source=. \
  --remote=origin \
  --push \
  --description "A retro modal terminal editor in Rust with Vim-inspired motions and SHA-256 integrity checks."

gh repo edit Zoel-Manchon/maat \
  --add-topic rust \
  --add-topic ratatui \
  --add-topic tui \
  --add-topic terminal \
  --add-topic text-editor \
  --add-topic vim \
  --add-topic modal-editor \
  --add-topic sha256 \
  --add-topic cybersecurity \
  --add-topic cli
```

## First release

After CI passes:

```bash
git tag -a v0.3.0 -m "Maat 0.3.0"
git push origin v0.3.0
gh release create v0.3.0 --generate-notes --title "Maat 0.3.0"
```

## Recommended GitHub settings

- Keep Issues enabled.
- Disable the wiki; the repository documentation is sufficient for now.
- Enable branch protection for `main` after the first successful CI run.
- Require the `Build and test` check before merging pull requests.
- Enable private vulnerability reporting.
