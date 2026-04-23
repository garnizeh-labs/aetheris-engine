const { chromium } = require('playwright');

(async () => {
  console.log('--- Aetheris Smoke Test Starting ---');
  const browser = await chromium.launch({ headless: true });
  
  try {
    // Client 1
    const context1 = await browser.newContext();
    const page1 = await context1.newPage();
    console.log('Client 1: Navigating to http://localhost:5173');
    await page1.goto('http://localhost:5173');
    
    // Client 2
    const context2 = await browser.newContext();
    const page2 = await context2.newPage();
    console.log('Client 2: Navigating to http://localhost:5173');
    await page2.goto('http://localhost:5173');

    // Login Client 1
    console.log('Client 1: Logging in...');
    await page1.fill('#auth-email', 'smoke-test-1@aetheris.dev');
    await page1.click('#btn-request-otp');
    await page1.fill('#auth-code', '000001');
    await page1.click('#btn-login-otp');
    
    // Login Client 2
    console.log('Client 2: Logging in...');
    await page2.fill('#auth-email', 'smoke-test-2@aetheris.dev');
    await page2.click('#btn-request-otp');
    await page2.fill('#auth-code', '000001');
    await page2.click('#btn-login-otp');

    // Wait for connections
    console.log('Waiting for "Connected" status...');
    await page1.waitForSelector('text=Connected:', { timeout: 60000 });
    await page2.waitForSelector('text=Connected:', { timeout: 60000 });

    const status1 = await page1.innerText('#status');
    const status2 = await page2.innerText('#status');
    console.log('Client 1 Status:', status1);
    console.log('Client 2 Status:', status2);

    if (status1.includes('Connected:') && status2.includes('Connected:')) {
      console.log('SUCCESS: Both clients connected successfully.');
    } else {
      throw new Error('FAILED: One or more clients failed to connect.');
    }

    // Verify canvas is visible (engine started)
    const canvas1Visible = await page1.isVisible('#engine-canvas');
    const canvas2Visible = await page2.isVisible('#engine-canvas');
    
    if (canvas1Visible && canvas2Visible) {
      console.log('SUCCESS: Engine canvas visible on both clients.');
    } else {
      throw new Error('FAILED: Engine canvas not visible.');
    }

    console.log('--- Smoke Test PASSED ---');
  } catch (err) {
    console.error('--- Smoke Test FAILED ---');
    console.error(err);
    process.exit(1);
  } finally {
    await browser.close();
  }
})();
