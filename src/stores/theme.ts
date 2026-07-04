import { defineStore } from "pinia";
import { ref, watch } from "vue";

export type ThemeMode = "light" | "dark" | "auto";

export const useThemeStore = defineStore("theme", () => {
  const stored = localStorage.getItem("theme");
  const initialMode: ThemeMode =
    stored === "light" || stored === "dark" || stored === "auto" ? stored : "auto";
  const mode = ref<ThemeMode>(initialMode);
  const systemDark = ref(window.matchMedia("(prefers-color-scheme: dark)").matches);

  // 监听系统主题变化
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (e) => {
    systemDark.value = e.matches;
  });

  const isDark = ref(mode.value === "dark" || (mode.value === "auto" && systemDark.value));

  watch([mode, systemDark], () => {
    isDark.value = mode.value === "dark" || (mode.value === "auto" && systemDark.value);
    document.documentElement.classList.toggle("dark", isDark.value);
    localStorage.setItem("theme", mode.value);
  }, { immediate: true });

  function setMode(m: ThemeMode) {
    mode.value = m;
  }

  return { mode, isDark, setMode };
});
