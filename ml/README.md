# ML Arbitrage Prediction System

## Status: COLLECTING DATA (Phase 1)

## Architecture

```
[Rust Bot - OBSERVE MODE]     [Python Collector]       [Python Trainer]
   detects swaps                reads opportunities      trains model
   simulates locally     --->   simulates on sequencer   on labeled data
   writes jsonl                 labels pass/fail    
   NO tx sent                   writes training_data     
                                                         [Scorer Service]
                                                         HTTP :8090
                                                         bot calls /score
                                                         before executing
```

## Files

- `collector.py` — Enriches raw opportunities with sequencer sim results
- `train.py` — Trains LightGBM model on labeled data  
- `scorer.py` — HTTP service the bot calls to check ML prediction
- `model.txt` — Trained model (after Phase 2)
- `training_data.jsonl` — Labeled training data
- `model_stats.json` — Model performance metrics

## Phases

### Phase 1: Data Collection (CURRENT)
Bot runs in ML_OBSERVE_ONLY=true mode. Detects opportunities, collector labels them.
Need: ~1000+ positive labels (sequencer sim passes) for good model.
Duration: depends on market activity (hours to days).

### Phase 2: Model Training  
```bash
python3 ml/train.py
```
Target: >90% precision (minimize false positives = minimize reverts)

### Phase 3: Deploy Scorer + Enable Live Mode
```bash
# Start scorer
nohup python3 ml/scorer.py &

# Switch bot to live mode with ML gating
# Edit .env: ML_OBSERVE_ONLY=false
sudo systemctl restart arbitrum-bot
```

### Phase 4: Continuous Learning
Bot executes, collector logs outcomes, model retrains periodically.

## Switching to LIVE mode
1. Check model stats: `cat ml/model_stats.json`
2. Verify precision > 90%
3. Start scorer: `python3 ml/scorer.py &`
4. Edit service: `ML_OBSERVE_ONLY=false`
5. Restart: `sudo systemctl restart arbitrum-bot`
