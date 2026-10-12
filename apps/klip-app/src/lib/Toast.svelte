<!-- The current toast (lib/toasts.svelte.ts), bottom centre above the status bar. -->
<script lang="ts">
  import { dismissToast, toasts } from './toasts.svelte';
</script>

<div class="toasts" role="status" aria-live="polite">
  {#if toasts.current}
    {#key toasts.current.id}
      <button class="toast" class:error={toasts.current.kind === 'error'} onclick={dismissToast} title="Dismiss">
        {toasts.current.text}
      </button>
    {/key}
  {/if}
</div>

<style>
  .toasts {
    position: fixed;
    left: 50%;
    bottom: calc(var(--statusbar-h, 0px) + 16px);
    transform: translateX(-50%);
    z-index: 50;
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    max-width: min(560px, calc(100vw - 32px));
    padding: 8px 14px;
    border: 1px solid var(--border-strong);
    border-left: 3px solid var(--good);
    border-radius: var(--radius);
    background: var(--panel-raised);
    color: var(--text-strong);
    font-size: var(--text-md);
    box-shadow: 0 6px 24px rgb(0 0 0 / 35%);
    animation: rise var(--motion, 160ms) ease-out;
    cursor: default;
  }
  .toast.error {
    border-left-color: var(--bad);
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
  }
</style>
