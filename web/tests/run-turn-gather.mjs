import { createRequire } from "node:module";

const packagePath = process.env.PLAYWRIGHT_PACKAGE;
const executablePath = process.env.CHROME_EXECUTABLE;
const url = process.env.TURN_TEST_URL;
if (!packagePath || !executablePath || !url) {
  throw new Error("PLAYWRIGHT_PACKAGE, CHROME_EXECUTABLE and TURN_TEST_URL are required");
}

const require = createRequire(import.meta.url);
const { chromium } = require(packagePath);
const browser = await chromium.launch({
  executablePath,
  headless: true,
  args: ["--no-sandbox", "--disable-gpu", "--disable-background-networking"],
});
try {
  const page = await browser.newPage();
  await page.goto(url);
  await page.waitForFunction(() => {
    try {
      const result = JSON.parse(document.querySelector("#result").textContent);
      return result.ok || result.error;
    } catch {
      return false;
    }
  }, null, { timeout: 15_000 });
  const result = JSON.parse(await page.locator("#result").textContent());
  console.log(JSON.stringify(result));
  if (!result.ok) process.exitCode = 1;
} finally {
  await browser.close();
}
