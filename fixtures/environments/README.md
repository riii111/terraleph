# Multi-environment demo

Run from the repository root:

```sh
python3 fixtures/demo.py multi
```

Compare groups the `server[*]` updates across `dev`, `stg`, and `prod` (2/2/4
instances) and marks their computed `output` as `[unknown values]`. The `dev`
only `dev_only` resource stays separate. Relations shows the configuration
reference from `server[0]` to `api`, plus a dotted block-level reference from
`api` to `dev_only` in `dev`. These links describe configuration evidence, not
change causes or apply order; the `server[0]` link may cover only part of the
grouped `server[*]` node.

Press `2`/`3` to focus Compare/Relations, `[`/`]` to change environment, and
`↑`/`↓` to select rows. Press `Space` to expand a selected summary or group,
`Enter` to open the plan, `Esc` to return to Overview, and `q` to quit.

Run the cloudless CLI acceptance with Terraform or OpenTofu after building:

```sh
cargo build --locked
python3 fixtures/environments/acceptance.py --binary target/debug/terraleph --tool terraform
python3 fixtures/environments/acceptance.py --binary target/debug/terraleph --tool tofu
```
