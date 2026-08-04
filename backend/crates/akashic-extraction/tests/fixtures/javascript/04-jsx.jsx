import React from 'react';

export function Badge({ label }) {
  const text = format(label);
  return <span className="badge">{text}</span>;
}

function format(value) {
  return String(value).toUpperCase();
}
