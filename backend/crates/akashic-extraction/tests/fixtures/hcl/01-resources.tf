provider "aws" {
  region = "us-east-1"
}

resource "aws_instance" "web" {
  ami           = "ami-123456"
  instance_type = "t3.micro"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_security_group" "sg" {
  name = "web-sg"

  ingress {
    from_port = 443
    to_port   = 443
    protocol  = "tcp"
  }
}
