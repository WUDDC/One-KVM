<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { NativeSelect } from '@/components/ui/native-select'
import { Spinner } from '@/components/ui/spinner'
import { Send, Radio, Settings2, ChevronLeft, ChevronRight, Play, Pause } from 'lucide-vue-next'
import { useRouter } from 'vue-router'
import { irApi, type IrRemote } from '@/api'

const { t } = useI18n()
const router = useRouter()

const remotes = ref<IrRemote[]>([])
const selectedRemoteId = ref<number | null>(null)
const loading = ref(true)
const unavailable = ref(false)
const sendingId = ref<number | null>(null)
const activeIndex = ref(-1)
const autoCycle = ref(false)
const cycleIntervalMs = ref(1000)
const intervalInput = ref('1')
let cycleTimer: number | null = null

/// Snap the interval to 0.5s steps clamped to [1, 99] seconds
/// (e.g. 1.9 -> 2, 1.4 -> 1.5) and apply it.
function normalizeInterval(e?: Event) {
  const raw = e?.target instanceof HTMLInputElement ? e.target.value : intervalInput.value
  const parsed = parseFloat(raw)
  const sec = Number.isFinite(parsed) ? Math.round(parsed * 2) / 2 : 1
  const clamped = Math.min(99, Math.max(1, sec))
  const text = String(clamped)
  intervalInput.value = text
  if (e?.target instanceof HTMLInputElement) e.target.value = text
  cycleIntervalMs.value = Math.round(clamped * 1000)
}

const selectedRemote = computed(
  () => remotes.value.find((r) => r.id === selectedRemoteId.value) ?? null,
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
  if (unavailable.value || (selectedRemote.value?.buttons.length ?? 0) === 0) return
  normalizeInterval()
  autoCycle.value = true
  cycleButton(1)
  cycleTimer = window.setInterval(() => cycleButton(1), cycleIntervalMs.value)
}

watch(cycleIntervalMs, () => {
  // Restart the timer so a changed interval applies while running.
  if (autoCycle.value) {
    stopCycleTimer()
    cycleTimer = window.setInterval(() => cycleButton(1), cycleIntervalMs.value)
  }
})

watch(selectedRemoteId, () => {
  activeIndex.value = -1
  stopAutoCycle()
})

watch(unavailable, (v) => {
  if (v) stopAutoCycle()
})

onUnmounted(stopAutoCycle)

async function load() {
  loading.value = true
  try {
    const [remoteResponse, hardware] = await Promise.all([
      irApi.listRemotes(),
      irApi.hardware(),
    ])
    remotes.value = remoteResponse.remotes
    unavailable.value = !hardware.rx_available && !hardware.tx_available
    if (selectedRemoteId.value === null) {
      const first = remotes.value[0]
      if (first) selectedRemoteId.value = first.id
    }
  } catch {
    unavailable.value = true
  } finally {
    loading.value = false
  }
}

async function send(buttonId: number) {
  if (sendingId.value !== null) return
  sendingId.value = buttonId
  try {
    await irApi.send(buttonId)
  } catch {
    // Errors surface through the shared request toast.
  } finally {
    setTimeout(() => {
      if (sendingId.value === buttonId) sendingId.value = null
    }, 300)
  }
}

/// Move the active pointer through the current remote's buttons (wraps
/// around) and fire the button it lands on.
function cycleButton(dir: 1 | -1) {
  const buttons = selectedRemote.value?.buttons ?? []
  if (buttons.length === 0 || unavailable.value || sendingId.value !== null) return
  const base = activeIndex.value === -1 ? (dir > 0 ? -1 : 0) : activeIndex.value
  const next = buttons[(base + dir + buttons.length) % buttons.length]
  if (!next) return
  activeIndex.value = buttons.indexOf(next)
  send(next.id)
}

onMounted(load)
</script>

<template>
  <div class="p-3 space-y-3">
    <div class="flex items-center gap-2">
      <Radio class="size-4 text-muted-foreground shrink-0" />
      <NativeSelect
        v-model="selectedRemoteId"
        class="h-8 text-xs"
        :disabled="remotes.length === 0"
      >
        <option v-if="remotes.length === 0" :value="null" disabled>
          {{ t('ir.noRemotes') }}
        </option>
        <option v-for="remote in remotes" :key="remote.id" :value="remote.id">
          {{ remote.name }}
        </option>
      </NativeSelect>
    </div>

    <Spinner v-if="loading" class="mx-auto my-6 size-5" />

    <template v-else>
      <div v-if="unavailable" class="text-[11px] text-muted-foreground text-center py-1">
        {{ t('ir.unavailable') }}
      </div>

      <div v-if="remotes.length === 0" class="text-xs text-muted-foreground text-center py-4 space-y-2">
        <p>{{ t('ir.noRemotesHint') }}</p>
      </div>

      <div v-else-if="selectedRemote && selectedRemote.buttons.length === 0" class="text-xs text-muted-foreground text-center py-4">
        {{ t('ir.noButtons') }}
      </div>

      <template v-else-if="selectedRemote">
        <div class="flex items-center justify-center gap-1 flex-nowrap">
          <Button
            variant="outline"
            size="sm"
            class="h-8 shrink-0 px-2 text-xs"
            :disabled="unavailable || selectedRemote.buttons.length === 0 || sendingId !== null"
            :aria-label="t('ir.prevButton')"
            @click="cycleButton(-1)"
          ><ChevronLeft class="size-3.5 mr-0.5" />{{ t('ir.prevButton') }}</Button>
          <Button
            :variant="autoCycle ? 'default' : 'outline'"
            size="sm"
            class="h-8 shrink-0 px-2 text-xs"
            :disabled="unavailable || selectedRemote.buttons.length === 0"
            :aria-label="t('ir.autoCycle')"
            @click="toggleAutoCycle"
          >
            <Pause v-if="autoCycle" class="size-3 mr-0.5" />
            <Play v-else class="size-3 mr-0.5" />
            {{ t('ir.autoCycle') }}
          </Button>
          <div class="flex w-20 shrink-0 items-center gap-0.5">
            <Input
              v-model="intervalInput"
              type="number"
              min="1"
              max="99"
              step="0.5"
              inputmode="decimal"
              class="h-8 text-xs text-center"
              :aria-label="t('ir.autoCycleInterval')"
              @blur="normalizeInterval"
              @keyup.enter="($event.target as HTMLInputElement).blur()"
            />
            <span class="text-xs text-muted-foreground shrink-0">s</span>
          </div>
          <Button
            variant="outline"
            size="sm"
            class="h-8 shrink-0 px-2 text-xs"
            :disabled="unavailable || selectedRemote.buttons.length === 0 || sendingId !== null"
            :aria-label="t('ir.nextButton')"
            @click="cycleButton(1)"
          >{{ t('ir.nextButton') }}<ChevronRight class="size-3.5 ml-0.5" /></Button>
        </div>

        <div class="grid grid-cols-3 gap-1.5">
          <Button
            v-for="(button, index) in selectedRemote.buttons"
            :key="button.id"
            variant="outline"
            size="sm"
            class="h-auto min-h-12 flex-col gap-0.5 px-1 py-1.5 text-xs"
            :class="index === activeIndex && 'border-primary ring-1 ring-primary'"
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
