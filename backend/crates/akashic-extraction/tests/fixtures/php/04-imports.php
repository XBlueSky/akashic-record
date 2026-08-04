<?php

namespace App\Http {
    use App\Models\User;
    use App\Models\Post as Article;
    use App\Support\{Arr, Str};
    use function App\Helpers\format_name;

    class Controller
    {
        public function index(): User
        {
            return new User();
        }
    }
}
