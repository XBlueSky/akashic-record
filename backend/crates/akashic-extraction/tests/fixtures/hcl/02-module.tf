module "vpc" {
  source     = "./modules/vpc"
  cidr_block = "10.0.0.0/16"
}

module "network" {
  source  = "terraform-aws-modules/vpc/aws"
  version = "5.0.0"
  name    = "prod"
}
