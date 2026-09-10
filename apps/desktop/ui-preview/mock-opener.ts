export async function openUrl(url: string): Promise<void> {
  console.info("[preview] openUrl", url);
  window.open(url, "_blank", "noopener");
}
