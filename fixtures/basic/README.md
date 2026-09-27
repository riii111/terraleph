# Single-environment demo and plan checks

Run the single-environment TUI demo from the repository root:

```sh
python3 fixtures/demo.py single
```

The demo prepares a cloudless local `terraform_data` plan, builds Terraleph,
and removes its temporary Git repository and state after the TUI exits.

The fixture acceptance verifies the saved plan actions and Git changes, then
checks that a real `time_sleep` provider plan contains two known repeated-resource
updates. Run it with either supported CLI:

```sh
python3 fixtures/basic/plan.py accept --tool terraform
python3 fixtures/basic/plan.py accept --tool tofu
```
