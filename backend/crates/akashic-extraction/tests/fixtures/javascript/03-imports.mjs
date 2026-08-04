import defaultExport from 'some-package';
import { foo, bar as baz } from './utils.js';
import * as ns from './ns.js';
import Both, { extra } from './both.js';
import './side-effect.js';

export function run() {
  foo();
  baz();
  defaultExport();
}
