using System;
using SB = System.Text.StringBuilder;

namespace MyApp.Models
{
    /// <summary>Lifecycle states for a task.</summary>
    public enum TaskState
    {
        Pending,
        Running,
        Completed
    }
}
