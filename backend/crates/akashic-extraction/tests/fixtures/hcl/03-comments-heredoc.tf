# A hash comment before the resource.
// A double-slash comment too.
/* A block comment
   spanning multiple lines { with a brace } that must not break parsing */
resource "aws_instance" "app" {
  ami = "ami-789" # trailing hash comment

  # The heredoc body contains braces that MUST stay opaque so block
  # matching is not thrown off by the `}` inside the script.
  user_data = <<-EOF
    #!/bin/bash
    if [ -d /opt ]; then
      echo "{ this is not a block }"
    fi
    export CONFIG='{"key": "value"}'
    EOF

  tags = {
    Name = "app"
  }
}
