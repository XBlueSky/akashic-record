<script lang="ts">
import { defineComponent } from 'vue';
import type { User } from './types';

export default defineComponent({
  setup(props) {
    return { name: props.user.name };
  },
});
</script>
