package com.example.app

import scala.collection.mutable
import scala.collection.mutable.{Map, ListBuffer}
import java.util.*

object Registry {
  def create(): mutable.Map[String, Int] = {
    mutable.Map.empty
  }
}
