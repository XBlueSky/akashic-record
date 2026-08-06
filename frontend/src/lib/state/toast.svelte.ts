export type ToastType = "error" | "warning" | "info";
export interface ToastAction {
	label: string;
	onClick: () => void;
}
export interface Toast {
	id: number;
	message: string;
	type: ToastType;
	action?: ToastAction;
}

class ToastState {
	items = $state<Toast[]>([]);
	#seq = 0;

	push(message: string, type: ToastType = "info", action?: ToastAction): number {
		const id = ++this.#seq;
		this.items = [...this.items, { id, message, type, action }];
		return id;
	}

	dismiss(id: number): void {
		this.items = this.items.filter((t) => t.id !== id);
	}
}

export const toast = new ToastState();

// Legacy-compat helpers — mirror the names that legacy toast.ts exported
// so ported call sites keep working without changes.
export function addToast(message: string, type: ToastType = "error", action?: ToastAction): void {
	toast.push(message, type, action);
}

export function dismissToast(id: number): void {
	toast.dismiss(id);
}
