<script setup>
import { ref, computed } from 'vue';
import { fetchUser } from './api';

const count = ref(0);
const doubled = computed(() => count.value * 2);

function increment() {
  count.value++;
}
</script>
