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

# Choose environments, or search deeper than 4 levels
terraleph --env-dir envs/prod --env-dir envs/stg
terraleph --max-depth 6
```

Run from your configuration directory, or a directory above it to compare environments. From above, nothing runs until you choose: press `p` to plan the selected environment or `P` to plan all of them, with init first when needed. In a plan review, press `a` to apply that environment's reviewed plan without re-planning.

To use Terraleph with your usual commands:

```sh
alias terraform='terraleph terraform'
alias tofu='terraleph tofu'
```

Without a system clipboard, such as over SSH, `y` sends the copy to your terminal with OSC 52. Inside tmux, this requires `set -g set-clipboard on`.

---

P.S. The day I named this tool, I had an Ethiopian coffee made from Heleph beans, and it really calmed me down. Later I found out "Heleph" means something like *change*. A Terraform plan is basically a list of changes, so the name just stuck.
