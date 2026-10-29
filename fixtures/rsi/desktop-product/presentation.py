"""Actual WebKit presentation preferences through the native Rust Settings bridge."""
import json


def open_appearance(script, until, open_details):
    open_details()
    until(lambda: script(r'const e=document.querySelector(".settings-modal:modal [aria-label=\"Settings / appearance / theme\"]");return e&&!e.disabled&&e.getClientRects().length>0'))


def verify(script, button, fill, until, screenshot, report, system_theme=None):
    results = []

    def theme(value, size):
        open_appearance(script, until, lambda: (button('Settings'), button('General')))
        script('const e=document.querySelector("select[aria-label=\\"Settings / appearance / theme\\"]");e.value=[...e.options].find(o=>o.textContent===arguments[0]).value;e.dispatchEvent(new Event("change",{bubbles:true}));return true', [value])
        fill('input[aria-label="Settings / appearance / content_font_size"]', str(size))
        button('Save settings')
        until(lambda: script('return document.documentElement.dataset.theme===arguments[0]&&document.documentElement.dataset.contentSize===String(arguments[1])', [value, size]))
        button('Close settings')
        until(lambda: script('return !document.querySelector(".settings-modal:modal")'))
        result = script('''
        const root=getComputedStyle(document.documentElement),composer=document.querySelector('.pane.selected .composer');
        const style=getComputedStyle(composer),text=getComputedStyle(composer.querySelector('textarea')),muted=getComputedStyle(composer.querySelector('.composer-hint'));
        const lum=c=>c.match(/[\\d.]+/g).slice(0,3).map(Number).map(n=>{n/=255;return n<=.04045?n/12.92:((n+.055)/1.055)**2.4}).reduce((sum,n,i)=>sum+n*[.2126,.7152,.0722][i],0);
        const contrast=(a,b)=>(Math.max(lum(a),lum(b))+.05)/(Math.min(lum(a),lum(b))+.05);
        return {theme:document.documentElement.dataset.theme,systemDark:matchMedia('(prefers-color-scheme:dark)').matches,scheme:root.colorScheme,font:text.fontSize,contrast:contrast(text.color,style.backgroundColor),secondaryContrast:contrast(muted.color,style.backgroundColor),controlRadius:getComputedStyle(document.querySelector('[data-testid="new-conversation"]')).borderRadius,compactRadius:getComputedStyle(document.querySelector('#settings-open')).borderRadius};
        ''')
        assert result['font'] == f'{size}px', result
        assert result['contrast'] >= 4.5 and result['secondaryContrast'] >= 4.5, result
        assert result['controlRadius'] == '12px' and result['compactRadius'] == '8px', result
        if value != 'system': assert result['scheme'] == value, result
        elif system_theme: assert result['systemDark'] == (system_theme == 'dark'), result
        results.append(result)
        screenshot(f'presentation-{value}.png')

    theme('dark', 17)
    theme('light', 12)
    theme('system', 14)
    button('Commands')
    until(lambda: script('return Boolean(document.querySelector(".command-palette:modal"))'))
    screenshot('presentation-commands.png')
    button('Close commands')
    until(lambda: script('return !document.querySelector(".command-palette")'))
    assert script('return document.activeElement?.getAttribute("aria-label") === "Commands"')
    (report / 'presentation.json').write_text(json.dumps(results, indent=2))
