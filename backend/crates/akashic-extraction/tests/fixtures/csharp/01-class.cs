using System;
using System.Collections.Generic;

namespace MyApp.Services
{
    /// <summary>
    /// A simple calculator class demonstrating method extraction.
    /// This class provides basic arithmetic operations.
    /// </summary>
    public class Calculator
    {
        private int _state;

        /// <summary>Constructs a Calculator with an initial state value.</summary>
        public Calculator(int initial)
        {
            _state = initial;
        }

        /// <summary>Adds two integers and returns the result.</summary>
        public int Add(int a, int b)
        {
            int result = a + b;
            return result;
        }

        /// <summary>A private helper that delegates to the public Add method.</summary>
        private int Helper()
        {
            return Add(1, 2);
        }

        /// <summary>Returns a string representation of the calculator.</summary>
        public override string ToString()
        {
            return _state.ToString();
        }
    }
}
