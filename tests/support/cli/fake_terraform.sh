#!/bin/sh
set -eu

printf '%s\n' "$(basename "$0")" >> "$TERRALEPH_FAKE_TOOL_LOG"
printf '%s|%s\n' "$PWD" "$*" >> "$TERRALEPH_FAKE_INVOCATIONS"
printf 'TF_CLI_ARGS=%s\n' "${TF_CLI_ARGS-}" >> "$TERRALEPH_FAKE_ENV_LOG"
printf 'TF_CLI_ARGS_plan=%s\n' "${TF_CLI_ARGS_plan-}" >> "$TERRALEPH_FAKE_ENV_LOG"
printf 'TF_CLI_ARGS_apply=%s\n' "${TF_CLI_ARGS_apply-}" >> "$TERRALEPH_FAKE_ENV_LOG"

case "${TERRALEPH_FAKE_MODE:-}" in
  env_*)
    case "$1" in
      workspace)
        printf '%s\n' "${TF_WORKSPACE:-default}"
        exit 0
        ;;
      init)
        if [ -f fail-init ]; then printf 'synthetic init failure\n' >&2; exit 1; fi
        mkdir -p .terraform
        printf '%s' '{"backend":{"type":"local"}}' > .terraform/terraform.tfstate
        : > initialized
        ;;
      plan)
        for argument in "$@"; do
          case "$argument" in -out=*) printf '%s\n' "${argument#-out=}" >> "$TERRALEPH_FAKE_PLAN_PATH.all" ;; esac
        done
        if [ -f warning-plan ]; then
          printf '%s\n' '{"type":"diagnostic","diagnostic":{"severity":"warning","summary":"synthetic plan warning","detail":"Review this provider warning"}}'
        fi
        if [ -f interrupt-plan ]; then exit 130; fi
        if [ -f require-init ]; then
          printf '%s\n' '{"type":"diagnostic","diagnostic":{"severity":"error","summary":"Backend initialization required","detail":"Run init"}}'
          if [ ! -f always-reinit ]; then rm require-init; fi
          exit 1
        fi
        if [ -f fail-plan ]; then
          printf '%s\n' '{"type":"diagnostic","diagnostic":{"severity":"error","summary":"Missing required variable","detail":"Pass a variable to retry this environment"}}'
          rm fail-plan
          exit 1
        fi
        if [ -f slow-plan ]; then
          for argument in "$@"; do
            case "$argument" in -out=*) printf '%s\n' "${argument#-out=}" > "$TERRALEPH_FAKE_PLAN_PATH" ;; esac
          done
          exec python3 -c 'import os,signal,sys,time; signal.signal(signal.SIGINT, lambda *_: (open(os.environ["TERRALEPH_FAKE_SIGNAL_LOG"], "a").write("plan_present=" + str(os.path.isfile(open(os.environ["TERRALEPH_FAKE_PLAN_PATH"]).read().strip())) + "\n"), time.sleep(0.1), sys.exit(130))); open(os.environ["TERRALEPH_FAKE_PID_PATH"], "w").write(str(os.getpid())); exec("while not os.path.isfile(\"release-plan\"):\n time.sleep(0.1)"); sys.exit(2)'
        fi
        ;;
    esac
    ;;
esac

case "$1" in
  version)
    printf '%s\n' '{"terraform_version":"1.9.0"}'
    ;;
  init)
    printf 'Initializing the backend...\n'
    printf 'Initializing provider plugins...\n' >&2
    if [ "${TERRALEPH_FAKE_MODE:-success}" = init_failure ]; then
      printf 'synthetic init failure\n' >&2
      exit 1
    fi
    if [ "${TERRALEPH_FAKE_MODE:-success}" = diagnostic_success ]; then
      printf '╷\n│ Warning: synthetic init warning\n│\n│ fake Terraform initialized with a warning\n╵\n' >&2
    fi
    ;;
  workspace)
    printf '%s\n' "${TERRALEPH_FAKE_WORKSPACE:-default}"
    ;;
  plan)
    plan_path=''
    previous=''
    for argument in "$@"; do
      case "$argument" in
        -out=*) plan_path=${argument#-out=} ;;
        -out) previous=out ;;
        *)
          if [ "$previous" = out ]; then
            plan_path=$argument
            previous=''
          fi
          ;;
      esac
    done
    printf '%s\n' "$plan_path" > "$TERRALEPH_FAKE_PLAN_PATH"
    : > "$plan_path"
    if [ "${TERRALEPH_FAKE_MODE:-success}" = interrupt ]; then
      exec python3 -c 'import os,signal,sys,time; signal.signal(signal.SIGINT, lambda *_: (open(os.environ["TERRALEPH_FAKE_SIGNAL_LOG"], "a").write("SIGINT\n"), time.sleep(1), sys.exit(130))); open(os.environ["TERRALEPH_FAKE_PID_PATH"], "w").write(f"{os.getpid()}\n"); time.sleep(30)'
    fi
    printf '%s\n' "$$" > "$TERRALEPH_FAKE_PID_PATH"
    if [ "${TERRALEPH_FAKE_MODE:-success}" = failure ]; then
      printf '%s\n' '{"type":"diagnostic","diagnostic":{"severity":"error","summary":"synthetic plan failure","detail":"fake Terraform failed"}}'
      exit 1
    fi
    if [ "${TERRALEPH_FAKE_MODE:-success}" = diagnostic_success ]; then
      printf '%s\n' '{"type":"diagnostic","diagnostic":{"severity":"warning","summary":"synthetic plan warning","detail":"fake Terraform completed with a warning"}}'
    fi
    if [ "${TERRALEPH_FAKE_MODE:-success}" = no_changes ]; then
      exit 0
    fi
    printf '%s\n' '{"type":"planned_change","change":{"resource":{"addr":"terraform_data.api"}}}'
    exit 2
    ;;
  apply)
    plan_path=''
    for argument in "$@"; do
      case "$argument" in
        *.tfplan) plan_path=$argument ;;
      esac
    done
    test -n "$plan_path"
    test -f "$plan_path"
    if [ "${TERRALEPH_FAKE_MODE:-success}" = apply_interrupt ] || [ "${TERRALEPH_FAKE_MODE:-success}" = signal_apply_term ]; then
      exec python3 -c 'import os,signal,sys,time; signal.signal(signal.SIGINT, lambda *_: (print("Stopping apply", flush=True), time.sleep(1), sys.exit(130))); open(os.environ["TERRALEPH_FAKE_PID_PATH"], "w").write(f"{os.getpid()}\n"); print("{\"type\":\"apply_start\",\"@message\":\"Applying saved plan...\",\"hook\":{\"resource\":{\"addr\":\"terraform_data.api\"},\"action\":\"update\"}}", flush=True); time.sleep(30)'
    fi
    printf '%s\n' "$$" > "$TERRALEPH_FAKE_PID_PATH"
    if [ "${TERRALEPH_FAKE_MODE:-success}" = apply_failure ]; then
      sleep 1
      printf '%s\n' '{"type":"apply_start","@message":"Applying saved plan...","hook":{"resource":{"addr":"terraform_data.api"},"action":"update"}}'
      printf '%s\n' '{"type":"diagnostic","@level":"error","diagnostic":{"severity":"error","summary":"synthetic apply failure","detail":"Changes may already be applied. must-not-be-logged","address":"terraform_data.api"}}'
      printf '%s\n' '{"type":"apply_errored","@message":"Apply failed","hook":{"resource":{"addr":"terraform_data.api"},"action":"update"}}'
      exit 1
    fi
    printf '%s\n' '{"type":"apply_start","@message":"Applying saved plan...","hook":{"resource":{"addr":"terraform_data.api"},"action":"update"}}'
    sleep 1
    printf '%s\n' '{"type":"apply_progress","@message":"terraform_data.api: Applying must-not-be-logged","hook":{"resource":{"addr":"terraform_data.api"},"action":"update"}}'
    printf '%s\n' '{"type":"apply_complete","@message":"terraform_data.api: Update complete","hook":{"resource":{"addr":"terraform_data.api"},"action":"update"}}'
    printf '%s\n' '{"type":"change_summary","@message":"Apply complete! Resources: 1 added, 1 changed, 0 destroyed.","changes":{"add":1,"change":1,"remove":0,"operation":"apply"}}'
    printf '%s\n' '{"type":"outputs","@message":"endpoint = \"https://example.test\""}'
    exit 0
    ;;
  show)
    case "${TERRALEPH_FAKE_MODE:-}" in env_*) printf "%s|%s\n" "$PWD" "$*" >> "$TERRALEPH_FAKE_PLAN_PATH.shows" ;; esac
    if [ "$2" = -json ]; then
      if [ -f invalid-show ]; then printf '%s\n' '{"format_version":"99.0"}'; exit 0; fi
      if [ "${TERRALEPH_FAKE_MODE:-success}" = no_changes ]; then
        printf '%s\n' '{"format_version":"1.0","applyable":false}'
      else
        cat "$TERRALEPH_FAKE_SHOW_JSON"
      fi
    else
      if [ "${TERRALEPH_FAKE_MODE:-success}" = no_changes ]; then
        printf '%s\n' 'No changes. Your infrastructure matches the configuration.'
      else
        cat "$TERRALEPH_FAKE_SHOW_TEXT"
      fi
    fi
    ;;
  *)
    exit 2
    ;;
esac
