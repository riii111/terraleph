# Terraleph ☕

Review and apply Terraform or OpenTofu plans in your terminal.

<img src="assets/demo.gif" alt="Comparing dev, stg, and prod plans, reviewing the prod plan, and applying it" width="800">

## Concept

> Terraform-native · Review before apply · Ephemeral UI

Review familiar plan output and apply the exact plan you reviewed. The UI appears when you need it, then returns you to your shell.

## Features

- **Plan review**: Full plan display, scrolling, and keyword filtering
- **Change overview**: Grouped resource changes and a relationship graph
- **Environment comparison**: Plans from multiple environments, side by side
- **Saved plan apply**: Target and workspace confirmation, with no replanning
- **Apply progress**: Resource status and logs
- **Clipboard**: Copy the full plan or apply results

<img src="https://github.com/user-attachments/assets/ba22d687-c023-4064-923e-d2be5658af11" alt="Change overview" width="600">

## Installation

```sh
# Homebrew
brew install riii111/terraleph/terraleph

# mise
mise use -g github:riii111/terraleph

# Cargo
cargo install --locked terraleph

# Nix
nix run github:riii111/terraleph
```

Prebuilt binaries are also available on the [Releases](https://github.com/riii111/terraleph/releases) page.

## Usage

Run in an interactive terminal with Terraform or OpenTofu installed and credentials configured. When a directory needs `init`, Terraleph runs it before the plan. It never passes `-migrate-state`, `-reconfigure`, or `-upgrade`; run `init` yourself when a change needs one of them.

```sh
# Open the change overview
terraleph

# Review a plan, then optionally apply it
terraleph plan

# Review and apply
terraleph apply

# Use OpenTofu
terraleph tofu plan
terraleph tofu apply
```

Run from your configuration directory, or from a directory above it, such as the repository root, to compare environments. From above, nothing runs until you choose: press `p` to plan the selected environment or `P` to plan all of them, with init first when needed. In a plan review, press `a` to apply that environment's reviewed plan without re-planning.

### Choosing environments

When the current directory has no configuration of its own, Terraleph searches up to 4 directory levels below it. A directory becomes a candidate when its configuration declares a `backend` or `cloud` block. The search skips:

- hidden directories such as `.terraform` and `.git`
- symlinked directories
- directories that another configuration calls as a local module (`source = "../modules/app"`)

Candidates are told apart by their path from the current directory. When the search misses an environment or offers the wrong ones, put Terraleph's options before the command:

```sh
# Plan these directories instead of searching
terraleph --env-dir live/prod --env-dir live/stg plan

# Search deeper
terraleph --max-depth 6
```

`--env-dir` accepts any directory with configuration, including one without a backend block, and is resolved from the directory Terraform runs in (the current directory, or `-chdir`). Named directories still wait for `p` or `P`. Arguments after the command are passed to Terraform unchanged.

Each directory is one environment. Terraleph does not split a directory into environments by workspace, backend configuration file, or variable file, and does not change those settings: each directory plans with the workspace it already uses and the `-var-file` options you pass. Comparison matches resources by address only, so a resource missing from one environment can come from differing configurations rather than drift.

To use Terraleph with your usual commands:

```sh
alias terraform='terraleph terraform'
alias tofu='terraleph tofu'
```

Without a system clipboard, such as over SSH, `y` sends the copy to your terminal with OSC 52. Inside tmux, this requires `set -g set-clipboard on`.

---

P.S. The day I named this tool, I had an Ethiopian coffee made from Heleph beans, and it really calmed me down. Later I found out "Heleph" means something like *change*. A Terraform plan is basically a list of changes, so the name just stuck.
