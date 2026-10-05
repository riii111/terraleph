variable "revision" { default = "review" }
resource "terraform_data" "drift" { input = var.revision }
resource "terraform_data" "api" { input = var.revision }
