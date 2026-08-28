<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { NativeSelect } from '@/components/ui/native-select'
import { Spinner } from '@/components/ui/spinner'
import { Send, Radio, Settings2, ChevronLeft, ChevronRight, Play, Square } from 'lucide-vue-next'
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

const selectedRemote = computed(
  () => remotes.value.find((r) => r.id === selectedRemoteId.value) ?? null,
)

watch(selectedRemoteId, () => {
  activeIndex.value = -1
})

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

// Auto-cycle: fire the next button every second until stopped.
const AUTO_CYCLE_INTERVAL_MS = 1000
const autoCycling = ref(false)
let autoTimer: number | null = null

function stopAutoCycle() {
  autoCycling.value = false
  if (autoTimer !== null) {
    clearInterval(autoTimer)
    autoTimer = null
  }
}

function toggleAutoCycle() {
  if (autoCycling.value) {
    stopAutoCycle()
    return
  }
  if (unavailable.value || (selectedRemote.value?.buttons.length ?? 0) === 0) return
  autoCycling.value = true
  cycleButton(1)
  autoTimer = window.setInterval(() => {
    if (unavailable.value || (selectedRemote.value?.buttons.length ?? 0) === 0) {
      stopAutoCycle()
      return
    }
    if (sendingId.value === null) cycleButton(1)
  }, AUTO_CYCLE_INTERVAL_MS)
}

onUnmounted(stopAutoCycle)
watch(unavailable, (value) => {
  if (value) stopAutoCycle()
})

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
        <div class="flex items-center justify-center gap-2">
          <Button
            variant="outline"
            size="sm"
            class="h-7 text-xs"
            :disabled="unavailable || selectedRemote.buttons.length === 0 || sendingId !== null"
            :aria-label="t('ir.prevButton')"
            @click="cycleButton(-1)"
          ><ChevronLeft class="size-3.5 mr-0.5" />{{ t('ir.prevButton') }}</Button>
          <Button
            :variant="autoCycling ? 'default' : 'outline'"
            size="sm"
            class="h-7 text-xs"
            :disabled="unavailable || selectedRemote.buttons.length === 0"
            :aria-label="t('ir.autoCycle')"
            @click="toggleAutoCycle"
          >
            <Square v-if="autoCycling" class="size-3 mr-0.5" />
            <Play v-else class="size-3 mr-0.5" />
            {{ t('ir.autoCycle') }}
          </Button>
          <Button
            variant="outline"
            size="sm"
            class="h-7 text-xs"
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
