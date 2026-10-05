resource "terraform_data" "lock_holder" {
  provisioner "local-exec" {
    command = "python3 -c 'from pathlib import Path; import time; Path(\"lock-ready\").touch(); time.sleep(60)'"
  }
}
resource "terraform_data" "api" { input = "synthetic-lock-conflict" }
