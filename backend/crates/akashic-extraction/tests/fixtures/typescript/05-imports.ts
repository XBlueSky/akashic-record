import { foo, bar as baz } from './utils';
import defaultExport from 'some-package';
import * as ns from './ns';
import type { Type } from './types';
import { type Inline, regular } from './mixed';
import Both, { extra } from './both';
import './side-effect';

foo();
baz();
defaultExport();
ns.run();
