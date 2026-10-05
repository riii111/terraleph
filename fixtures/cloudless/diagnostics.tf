terraform {
  backend "local" {}
}
variable "valid" { default = true }
check "synthetic_warning" {
  assert {
    condition     = terraform_data.api.input == "expected"
    error_message = "Synthetic check warning: unexpected input."
  }
}
resource "terraform_data" "api" {
  input = "synthetic"
  lifecycle {
    precondition {
      condition     = var.valid
      error_message = "Synthetic precondition error: invalid environment."
    }
  }
}
