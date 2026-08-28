<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Spinner } from '@/components/ui/spinner'
import { Send, Settings2, ChevronLeft, ChevronRight, Play, Pause, Pin, PinOff } from 'lucide-vue-next'
import { useRouter } from 'vue-router'
import { irApi } from '@/api'

const { t } = useI18n()
const router = useRouter()

const emit = defineEmits<{ close: [] }>()

const pinned = ref(false)
const overlayOpacity = ref(100)

const loading = ref(true)
const unavailable = ref(false)
const sendingId = ref<number | null>(null)
const activeIndex = ref(-1)
const autoCycle = ref(false)
const cycleIntervalMs = ref(5000)
const intervalInput = ref('5')
let cycleTimer: number | null = null

const kvmRemote = ref<{ id: number; name: string; buttons: Array<{ id: number; name: string }> } | null>(null)
const buttons = computed(() => kvmRemote.value?.buttons ?? [])

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
  if (unavailable.value || buttons.value.length === 0) return
  normalizeInterval()
  autoCycle.value = true
  cycleButton(1)
  cycleTimer = window.setInterval(() => cycleButton(1), cycleIntervalMs.value)
}

watch(cycleIntervalMs, () => {
  if (autoCycle.value) {
    stopCycleTimer()
    cycleTimer = window.setInterval(() => cycleButton(1), cycleIntervalMs.value)
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
    kvmRemote.value = remoteResponse.remotes.find((r) => r.is_kvm) ?? null
    unavailable.value = !hardware.rx_available && !hardware.tx_available
  } catch {
    unavailable.value = true
    kvmRemote.value = null
  } finally {
    loading.value = false
  }
}

async function send(buttonId: number) {
  if (sendingId.value !== null) return
  sendingId.value = buttonId
  let ok = false
  try {
    await irApi.send(buttonId)
    ok = true
  } catch {
    // Errors surface through the shared request toast.
  } finally {
    setTimeout(() => {
      if (sendingId.value === buttonId) sendingId.value = null
    }, 300)
  }
  // Single-shot sends close the popover unless pinned or auto-cycling.
  if (ok && !autoCycle.value && !pinned.value) emit('close')
}

/// Move the active pointer through the buttons (wraps around) and fire.
function cycleButton(dir: 1 | -1) {
  const list = buttons.value
  if (list.length === 0 || unavailable.value || sendingId.value !== null) return
  const base = activeIndex.value === -1 ? (dir > 0 ? -1 : 0) : activeIndex.value
  const next = list[(base + dir + list.length) % list.length]
  if (!next) return
  activeIndex.value = list.indexOf(next)
  send(next.id)
}

onMounted(load)
</script>

<template>
  <div class="relative rounded-md border bg-popover shadow-md p-3 space-y-3" :style="{ opacity: pinned ? overlayOpacity / 100 : 1 }">
    <Spinner v-if="loading" class="mx-auto my-4 size-5" />

    <template v-else>
      <div v-if="unavailable" class="text-[11px] text-muted-foreground text-center py-1">
        {{ t('ir.unavailable') }}
      </div>

      <div v-if="!kvmRemote" class="text-xs text-muted-foreground text-center py-2">
        {{ t('ir.noKvmRemote') }}
      </div>

      <div v-else-if="buttons.length === 0" class="text-xs text-muted-foreground text-center py-2">
        {{ t('ir.noButtons') }}
      </div>

      <template v-else>
        <div class="flex items-center justify-center gap-1">
          <Button
            variant="ghost"
            size="icon-sm"
            :class="pinned && 'text-primary'"
            :aria-label="pinned ? t('ir.unpin') : t('ir.pin')"
            :title="pinned ? t('ir.unpin') : t('ir.pin')"
            @click="pinned = !pinned"
          >
            <Pin v-if="!pinned" class="size-4" />
            <PinOff v-else class="size-4" />
          </Button>
          <Button
            variant="outline"
            size="icon-sm"
            :disabled="unavailable || sendingId !== null"
            :aria-label="t('ir.prevButton')"
            @click="cycleButton(-1)"
          ><ChevronLeft class="size-4" /></Button>
          <Button
            :variant="autoCycle ? 'default' : 'outline'"
            size="sm"
            class="h-8 text-xs px-2.5"
            :disabled="unavailable"
            :aria-label="t('ir.cycleSwitch')"
            @click="toggleAutoCycle"
          >
            <Pause v-if="autoCycle" class="size-3 mr-0.5" />
            <Play v-else class="size-3 mr-0.5" />
            {{ t('ir.cycleSwitch') }}
          </Button>
          <div class="flex w-14 items-center gap-0.5">
            <Input
              v-model="intervalInput"
              type="number"
              min="1"
              max="99"
              step="0.5"
              inputmode="decimal"
              class="h-8 text-xs text-center px-1"
              :aria-label="t('ir.autoCycleInterval')"
              @blur="normalizeInterval"
              @keyup.enter="($event.target as HTMLInputElement).blur()"
            />
            <span class="text-xs text-muted-foreground shrink-0">s</span>
          </div>
          <Button
            variant="outline"
            size="icon-sm"
            :disabled="unavailable || sendingId !== null"
            :aria-label="t('ir.nextButton')"
            @click="cycleButton(1)"
          ><ChevronRight class="size-4" /></Button>
        </div>

        <div class="grid grid-cols-3 gap-1.5">
          <Button
            v-for="(button, index) in buttons"
            :key="button.id"
            variant="outline"
            size="sm"
            class="h-auto min-h-12 flex-col gap-0.5 px-1 py-1.5 text-xs"
            :class="index === activeIndex && '[outline:2px_solid_var(--primary)]! [outline-offset:-2px]!'"
            :disabled="unavailable || sendingId !== null"
            :title="unavailable ? t('ir.unavailable') : undefined"
            @click="activeIndex = index; send(button.id)"
          >
            <Spinner v-if="sendingId === button.id" class="size-3.5" />
            <Send v-else class="size-3.5 text-muted-foreground" />
            <span class="w-full truncate leading-tight">{{ button.name }}</span>
          </Button>
        </div>
      </template>
    </template>

    <Button
      variant="ghost"
      size="sm"
      class="w-full h-7 text-xs text-muted-foreground"
      @click="router.push('/settings?tab=ir')"
    >
      <Settings2 class="size-3.5 mr-1" />
      {{ t('ir.manageHint') }}
    </Button>
  </div>
</template>
