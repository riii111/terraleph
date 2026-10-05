terraform {
  backend "local" {}
  required_providers {
    random = { source = "hashicorp/random", version = "~> 3.7" }
  }
}
variable "revision" { default = "review" }
variable "secret" {
  type      = string
  sensitive = true
  default   = "synthetic-secret-never-use"
}
resource "random_password" "credential" { length = 24 }
resource "terraform_data" "nested" {
  input = {
    public   = var.revision
    accounts = [{ name = "synthetic", credentials = { token = var.secret, password = random_password.credential.result } }]
  }
}
output "secret" {
  value     = var.secret
  sensitive = true
}
output "nested" {
  value     = terraform_data.nested.output
  sensitive = true
}
