<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { NativeSelect } from '@/components/ui/native-select'
import { Spinner } from '@/components/ui/spinner'
import { Send, Radio, Settings2 } from 'lucide-vue-next'
import { useRouter } from 'vue-router'
import { irApi, type IrRemote } from '@/api'

const { t } = useI18n()
const router = useRouter()

const remotes = ref<IrRemote[]>([])
const selectedRemoteId = ref<number | null>(null)
const loading = ref(true)
const unavailable = ref(false)
const sendingId = ref<number | null>(null)

const selectedRemote = computed(
  () => remotes.value.find((r) => r.id === selectedRemoteId.value) ?? null,
)

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

onMounted(load)
</script>

<template>
  <div class="p-3 space-y-3">
    <div class="flex items-center gap-2">
      <Radio class="size-4 text-muted-foreground" />
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

      <div v-else-if="selectedRemote" class="grid grid-cols-3 gap-1.5">
        <Button
          v-for="button in selectedRemote.buttons"
          :key="button.id"
          variant="outline"
          size="sm"
          class="h-auto min-h-12 flex-col gap-0.5 px-1 py-1.5 text-xs"
          :disabled="unavailable || sendingId !== null"
          :title="unavailable ? t('ir.unavailable') : undefined"
          @click="send(button.id)"
        >
          <Spinner v-if="sendingId === button.id" class="size-3.5" />
          <Send v-else class="size-3.5 text-muted-foreground" />
          <span class="w-full truncate leading-tight">{{ button.name }}</span>
        </Button>
      </div>
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
