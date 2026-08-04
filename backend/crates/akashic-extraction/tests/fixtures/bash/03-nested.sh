#!/usr/bin/env bash

outer() {
  inner() {
    echo "nested"
  }
  inner
  ls
}
