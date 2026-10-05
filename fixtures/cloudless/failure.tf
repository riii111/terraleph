resource "terraform_data" "success" {
  count = 3
  input = "synthetic-success-${count.index}"
  provisioner "local-exec" {
    command = "python3 -c 'import time; time.sleep(${count.index + 1})'"
  }
}
resource "terraform_data" "failure" {
  depends_on = [terraform_data.success[0]]
  provisioner "local-exec" {
    command = "python3 -c 'import sys,time; time.sleep(2); sys.exit(\"synthetic apply failure\")'"
  }
}
