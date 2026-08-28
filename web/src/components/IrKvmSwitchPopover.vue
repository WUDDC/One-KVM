<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Spinner } from '@/components/ui/spinner'
import { ChevronLeft, ChevronRight, Play, Pause, Pin, PinOff } from 'lucide-vue-next'
import { irApi } from '@/api'

const { t } = useI18n()

const emit = defineEmits<{ close: [] }>()

const pinned = ref(false)
const overlayOpacity = ref(100)

const SLOT_COUNT = 8

const loading = ref(true)
const unavailable = ref(false)
// slot number (1-based) -> button id; only bound slots are present.
const slotButtons = ref<Record<number, { id: number; name: string }>>({})
const sendingSlot = ref<number | null>(null)
const activeSlot = ref<number | null>(null)
const autoCycle = ref(false)
const cycleIntervalMs = ref(5000)
const intervalInput = ref('5')
let cycleTimer: number | null = null

const boundSlots = computed(() =>
  Array.from({ length: SLOT_COUNT }, (_, i) => i + 1).filter((n) => slotButtons.value[n]),
)

function stopCycleTimer() {
  if (cycleTimer !== null) {
    clearInterval(cycleTimer)
    cycleTimer = null
  }
}

function stopAutoCycle() {
  stopCycleTimer()
  autoCycle.value = false
}

function toggleAutoCycle() {
  if (autoCycle.value) {
    stopAutoCycle()
    return
  }
  if (unavailable.value || boundSlots.value.length === 0) return
  normalizeInterval()
  autoCycle.value = true
  if (activeSlot.value === null || !slotButtons.value[activeSlot.value]) {
    const first = boundSlots.value[0]
    if (first === undefined) return
    activeSlot.value = first
  }
  const start = activeSlot.value
  if (start === null) return
  fire(start)
  cycleTimer = window.setInterval(() => cycleSlot(1), cycleIntervalMs.value)
}

watch(cycleIntervalMs, () => {
  if (autoCycle.value) {
    stopCycleTimer()
    cycleTimer = window.setInterval(() => cycleSlot(1), cycleIntervalMs.value)
  }
})

watch(unavailable, (v) => {
  if (v) stopAutoCycle()
})

onUnmounted(stopAutoCycle)

/// Snap the interval to 0.5s steps clamped to [1, 99] seconds.
function normalizeInterval(e?: Event) {
  const raw = e?.target instanceof HTMLInputElement ? e.target.value : intervalInput.value
  const parsed = parseFloat(raw)
  const sec = Number.isFinite(parsed) ? Math.round(parsed * 2) / 2 : 5
  const clamped = Math.min(99, Math.max(1, sec))
  const text = String(clamped)
  intervalInput.value = text
  if (e?.target instanceof HTMLInputElement) e.target.value = text
  cycleIntervalMs.value = Math.round(clamped * 1000)
}

async function load() {
  loading.value = true
  try {
    const [remoteResponse, hardware] = await Promise.all([
      irApi.listRemotes(),
      irApi.hardware(),
    ])
    const map: Record<number, { id: number; name: string }> = {}
    for (const remote of remoteResponse.remotes) {
      // Only the single KVM-switch remote drives the slot buttons.
      if (!remote.is_kvm) continue
      for (const button of remote.buttons) {
        if (button.slot !== null && button.slot >= 1 && button.slot <= SLOT_COUNT) {
          map[button.slot] = { id: button.id, name: button.name }
        }
      }
    }
    slotButtons.value = map
    unavailable.value = !hardware.rx_available && !hardware.tx_available
  } catch {
    unavailable.value = true
  } finally {
    loading.value = false
  }
}

async function fire(slot: number) {
  const button = slotButtons.value[slot]
  if (!button || sendingSlot.value !== null) return
  sendingSlot.value = slot
  let ok = false
  try {
    await irApi.send(button.id)
    ok = true
  } catch {
    // Errors surface through the shared request toast.
  } finally {
    setTimeout(() => {
      if (sendingSlot.value === slot) sendingSlot.value = null
    }, 300)
  }
  // Single-shot sends close the popover unless pinned or auto-cycling.
  if (ok && !autoCycle.value && !pinned.value) emit('close')
}

function onSlotClick(slot: number) {
  activeSlot.value = slot
  fire(slot)
}

/// Move through the *bound* slots in order (wraps around) and fire each.
function cycleSlot(dir: 1 | -1) {
  const bound = boundSlots.value
  if (bound.length === 0 || unavailable.value || sendingSlot.value !== null) return
  const pos = activeSlot.value !== null ? bound.indexOf(activeSlot.value) : -1
  const base = pos === -1 ? (dir > 0 ? -1 : 0) : pos
  const next = bound[(base + dir + bound.length) % bound.length]
  if (next === undefined) return
  activeSlot.value = next
  fire(next)
}

onMounted(load)
</script>

<template>
  <div class="relative p-3 space-y-3" :style="{ opacity: pinned ? overlayOpacity / 100 : 1 }">
    <Button
      variant="ghost"
      size="icon-sm"
      class="absolute top-1.5 left-1.5 z-10"
      :class="pinned && 'text-primary'"
      :aria-label="pinned ? t('ir.unpin') : t('ir.pin')"
      :title="pinned ? t('ir.unpin') : t('ir.pin')"
      @click="pinned = !pinned"
    >
      <Pin v-if="!pinned" class="size-4" />
      <PinOff v-else class="size-4" />
    </Button>
    <Spinner v-if="loading" class="mx-auto my-4 size-5" />

    <template v-else>
      <div v-if="unavailable" class="text-[11px] text-muted-foreground text-center py-1">
        {{ t('ir.unavailable') }}
      </div>

      <div v-if="boundSlots.length === 0" class="text-xs text-muted-foreground text-center py-2">
        {{ t('ir.noSlotBound') }}
      </div>

      <template v-else>
        <div class="flex items-center justify-center gap-1.5 flex-wrap pl-9">
          <Button
            v-for="slot in 8"
            :key="slot"
            variant="outline"
            size="icon"
            class="size-9 text-xs font-medium"
            :class="slot === activeSlot && '[outline:2px_solid_var(--primary)]! [outline-offset:-2px]!'"
            :disabled="!slotButtons[slot] || unavailable || sendingSlot !== null"
            :title="slotButtons[slot]
              ? `${slot}: ${slotButtons[slot].name}`
              : t('ir.slotEmpty')"
            @click="onSlotClick(slot)"
          >
            <Spinner v-if="sendingSlot === slot" class="size-3.5" />
            <span v-else>{{ slot }}</span>
          </Button>
          <Button
            variant="outline"
            size="icon-sm"
            :disabled="unavailable || boundSlots.length === 0 || sendingSlot !== null"
            :aria-label="t('ir.prevButton')"
            @click="cycleSlot(-1)"
          ><ChevronLeft class="size-4" /></Button>
          <Button
            variant="outline"
            size="icon-sm"
            :disabled="unavailable || boundSlots.length === 0 || sendingSlot !== null"
            :aria-label="t('ir.nextButton')"
            @click="cycleSlot(1)"
          ><ChevronRight class="size-4" /></Button>
          <Button
            :variant="autoCycle ? 'default' : 'outline'"
            size="sm"
            class="h-9 text-xs px-2.5"
            :disabled="unavailable || boundSlots.length === 0"
            :aria-label="t('ir.cycleSwitch')"
            @click="toggleAutoCycle"
          >
            <Pause v-if="autoCycle" class="size-3.5 mr-1" />
            <Play v-else class="size-3.5 mr-1" />
            {{ t('ir.cycleSwitch') }}
          </Button>
          <div class="flex w-20 items-center gap-0.5">
            <Input
              v-model="intervalInput"
              type="number"
              min="1"
              max="99"
              step="0.5"
              inputmode="decimal"
              class="h-9 text-xs text-center"
              :aria-label="t('ir.autoCycleInterval')"
              @blur="normalizeInterval"
              @keyup.enter="($event.target as HTMLInputElement).blur()"
            />
            <span class="text-xs text-muted-foreground shrink-0">s</span>
          </div>
        </div>

        <div v-if="pinned" class="flex items-center gap-2">
          <span class="text-xs text-muted-foreground shrink-0">{{ t('ir.overlayOpacity') }}</span>
          <input
            type="range"
            min="30"
            max="100"
            v-model.number="overlayOpacity"
            class="flex-1 accent-primary"
            :aria-label="t('ir.overlayOpacity')"
          />
          <span class="text-xs text-muted-foreground w-9 text-right shrink-0">{{ overlayOpacity }}%</span>
        </div>
      </template>
    </template>
  </div>
</template>
