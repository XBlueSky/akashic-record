<?php

namespace App\Model {
    enum Suit: string
    {
        case Hearts = 'H';
        case Diamonds = 'D';
        case Clubs = 'C';
        case Spades = 'S';

        public function color(): string
        {
            return $this->isRed() ? "red" : "black";
        }

        public function isRed(): bool
        {
            return $this === Suit::Hearts || $this === Suit::Diamonds;
        }
    }

    function describe_suit(Suit $suit): string
    {
        return $suit->color();
    }
}
