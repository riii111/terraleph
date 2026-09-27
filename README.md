# Terraleph ☕

Review and apply Terraform or OpenTofu plans in your terminal.

<img src="https://github.com/user-attachments/assets/ba22d687-c023-4064-923e-d2be5658af11" alt="overview" width="800">

<details>
<summary>More screenshots</summary>

**Full plan review**

<img src="https://github.com/user-attachments/assets/425c636b-ea71-4271-887f-3997f6911b47" alt="overview" width="800">

<img src="https://github.com/user-attachments/assets/1e248721-aa3b-44b4-a0ae-ae8d78cbd796" alt="overview" width="800">

</details>

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

## Usage

Run in an interactive terminal with Terraform or OpenTofu initialized and credentials configured.

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

Run from your configuration directory, or its parent to compare environments. In a plan review, press `a` to apply that environment's reviewed plan without re-planning.

To use Terraleph with your usual commands:

```sh
alias terraform='terraleph terraform'
alias tofu='terraleph tofu'
```

---

P.S. The day I named this tool, I had an Ethiopian coffee made from Heleph beans, and it really calmed me down. Later I found out "Heleph" means something like *change*. A Terraform plan is basically a list of changes, so the name just stuck.
