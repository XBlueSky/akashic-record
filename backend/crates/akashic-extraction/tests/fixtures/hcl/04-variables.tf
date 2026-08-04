terraform {
  required_version = ">= 1.5.0"
}

variable "region" {
  type    = string
  default = "us-east-1"
}

output "instance_ip" {
  value = "10.0.0.5"
}

locals {
  common_tags = "managed-by-terraform"
}

data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"]
}
