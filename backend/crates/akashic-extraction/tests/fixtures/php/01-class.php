<?php

namespace App\Services {
    /**
     * A simple calculator class demonstrating method extraction.
     */
    class Calculator
    {
        private int $state;

        public function __construct(int $initial)
        {
            $this->state = $initial;
        }

        public function add(int $a, int $b): int
        {
            $result = $a + $b;
            return $result;
        }

        private function helper(): int
        {
            return $this->add(1, 2);
        }

        public static function version(): string
        {
            return self::release();
        }

        protected static function release(): string
        {
            return "1.0";
        }
    }
}
