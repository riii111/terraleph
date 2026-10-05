variable "revision" { default = "review" }
resource "terraform_data" "server" {
  count = 300
  input = {
    revision    = var.revision
    group       = "group-${floor(count.index / 25)}"
    description = join("", [for n in range(40) : "synthetic attribute ${count.index}/${n}; "])
  }
}
