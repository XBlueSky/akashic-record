# Reference-edge fixture: resource / data / module refs plus skip cases.

resource "aws_subnet" "main" {
  cidr_block = "10.0.1.0/24"
}

data "aws_ami" "ubuntu" {
  most_recent = true
}

module "network" {
  source = "./modules/network"
}

variable "env" {
  type    = string
  default = "prod"
}

locals {
  prefix = "app"
}

resource "aws_instance" "web" {
  # Resource reference → Call edge to aws_subnet.main (ref_kind=resource).
  subnet_id = aws_subnet.main.id

  # Data reference → Call edge to aws_ami.ubuntu (ref_kind=data).
  ami = data.aws_ami.ubuntu.id

  # Module reference → Call edge to network (ref_kind=module).
  vpc_id = module.network.vpc_id

  # Reserved scopes + function call → NO edges.
  instance_type = var.env
  name_prefix   = local.prefix
  root_path     = path.module
  count         = each.key
  user_data     = jsonencode({ key = "value" })

  # Interpolated reference inside a string → still a resource ref (dedup: the
  # second aws_subnet.main mention does NOT add another edge).
  tags = {
    Subnet = "${aws_subnet.main.id}"
  }
}
