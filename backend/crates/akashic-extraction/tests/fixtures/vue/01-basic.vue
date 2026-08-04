<script>
import { foo } from './utils';

export default {
  name: 'BasicComponent',
  data() {
    return { count: 0 };
  },
};
</script>

<template>
  <div>{{ count }}</div>
</template>
