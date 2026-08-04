<?php

namespace App\Contracts {
    interface Shape
    {
        public function area(): float;

        public function name(): string;
    }

    trait Describable
    {
        public function describe(): string
        {
            return $this->name();
        }

        protected function tag(): string
        {
            return "shape";
        }
    }
}
