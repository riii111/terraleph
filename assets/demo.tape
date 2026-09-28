# README demo. Render with assets/demo.sh, which composites the window over a generated wallpaper.
# Uses the cloudless dev/stg/prod scenario from fixtures/environments (terraform_data + local state only).
# The hidden terraform wrapper only paces `apply -json` events so the progress screen is visible.
# assets/social-preview.sh reuses the first Hide...Show block and the last Hide block.

Require terraform
Require python3
Require cargo

Set Shell bash
Set FontFamily "Menlo"
Set FontSize 13
Set LineHeight 1.2
Set Width 1200
Set Height 780
Set Padding 16
# assets/demo.sh keys out MarginFill outside the window; keep its geometry in sync.
Set Margin 20
Set MarginFill "#808080"
Set WindowBar Colorful
Set BorderRadius 10
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set TypingSpeed 120ms

# 0. Prepare the scenario and open the Overview off screen.
# The scenario runs as "infra" so the header does not show the random temp name.
Hide
Type@1ms `export PS1='$ ' REPO="$PWD" DEMO_REAL_TF="$(command -v terraform)" && unset CI`
Enter
Type@1ms `CARGO_TARGET_DIR="$REPO/target/demo" RUSTC_WRAPPER= cargo build --locked -q && S=$(cd fixtures && python3 -c 'from environments import environment as e; print(e.setup(ready=True))') && R=$(mktemp -d) && D="$R/infra" && mv "$S" "$D" && mkdir "$D/.bin" && cd "$D"`
Enter
Type@1ms `printf '%s\n' '#!/usr/bin/env python3' 'import os, subprocess, sys, time' 'real = os.environ["DEMO_REAL_TF"]' 'if "apply" not in sys.argv: os.execv(real, [real, *sys.argv[1:]])' 'child = subprocess.Popen([real, *sys.argv[1:]], stdout=subprocess.PIPE, text=True)' 'for line in child.stdout:' '    time.sleep(0.4 if "apply_start" in line or "apply_complete" in line else 0)' '    sys.stdout.write(line); sys.stdout.flush()' 'sys.exit(child.wait())' > .bin/terraform && chmod +x .bin/terraform`
Enter
Type@1ms `export PATH="$D/.bin:$REPO/target/demo/debug:$PATH" TF_CLI_CONFIG_FILE=/dev/null CHECKPOINT_DISABLE=1 && clear && terraleph terraform -chdir="$D" plan`
Enter
Wait+Screen@180s /Same change across envs/
Sleep 1s
Show

# 1. Overview: compare dev/stg/prod, show the Relations highlight following the
# selected row, then maximize Compare so the expanded server[*] rows all fit.
Sleep 2s
Type "2"
Sleep 400ms
Space
Sleep 800ms
Down
Sleep 1000ms
Down
Sleep 1000ms
Type "f"
Sleep 600ms
Space
Sleep 2200ms
# 3 restores the split layout and focuses Relations.
Type "3"
Sleep 1200ms

# 2. Plan detail: switch Relations to prod, open its plan, scroll, search.
Type "]"
Sleep 500ms
Type "]"
Sleep 1200ms
Enter
Sleep 1500ms
Down@300ms 5
Sleep 700ms
Type "/"
Sleep 400ms
Type@150ms "server"
Sleep 1800ms
Escape
Sleep 800ms

# 3. Apply confirmation.
Type "a"
Sleep 1500ms
Type@150ms "prod"
Sleep 600ms
Enter

# 4. Apply progress and result.
Wait+Screen@60s /Apply complete/
Sleep 2s

# 5. Close the UI (multi-environment sessions ask once more) and return to the shell.
Type "q"
Sleep 700ms
Enter
Sleep 1500ms

Hide
Type@1ms `cd "$REPO/fixtures" && mv "$D" "$S" && rmdir "$R" && python3 -c 'import sys; from pathlib import Path; from environments import environment as e; e.clean(Path(sys.argv[1]))' "$S" && cd "$REPO" && echo demo-cleaned`
Enter
Wait+Screen@30s /demo-cleaned/
